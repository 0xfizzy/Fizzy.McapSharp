using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ReaderBoundaryTests
{
    static byte[] Recording(McapCompression compression = McapCompression.None, bool complete = true)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, CompressionThreads = 0, UseChunks = complete }, true))
        {
            writer.RegisterSchema(7, "schema", "raw", [9, 8]);
            writer.RegisterSchema(8, "unused", "raw", [7]);
            writer.RegisterChannel(11, "topic", "raw", 7);
            writer.WriteMessage(new(11, 1, 2, 3), [1, 2, 3]);
            if (complete) writer.Complete(); else writer.Flush();
        }
        return stream.ToArray();
    }

    [Theory]
    [InlineData(0)] [InlineData(1)] [InlineData(2)]
    public void CompositeRandomAccessValidationFailureTerminatesSession(int invalid)
    {
        using var stream = new MemoryStream(Recording());
        using var reader = McapFileReader.OpenRecords(stream);
        var chunk = Assert.Single(reader.GetSummary()!.ChunkIndexes);
        if (invalid == 0)
            Assert.Throws<McapException>(() => reader.ReadChunk(chunk with { ChunkLength = chunk.ChunkLength + 1 }));
        else
        {
            var offsets = invalid == 1
                ? new Dictionary<ushort, ulong> { [11] = chunk.ChunkStartOffset }
                : new Dictionary<ushort, ulong> { [12] = chunk.MessageIndexOffsets[11] };
            Assert.Throws<McapException>(() => reader.ReadMessageIndexes(chunk with { MessageIndexOffsets = offsets }));
        }
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecord(new byte[4096], out _, out _));
        Assert.Throws<InvalidOperationException>(() => reader.GetSummary());
    }

    [Fact]
    public void RandomAccessArgumentRejectionDoesNotFailSession()
    {
        using var stream = new MemoryStream(Recording());
        using var reader = McapFileReader.OpenRecords(stream);
        Assert.Throws<ArgumentNullException>(() => reader.ReadChunk(null!));
        Assert.Throws<ArgumentNullException>(() => reader.ReadMessageIndexes(null!));
        Assert.Equal(McapReadStatus.Success, reader.ReadNextRecord(new byte[4096], out var opcode, out _));
        Assert.Equal(1, opcode);
    }

    [Theory]
    [InlineData(McapBufferReadMode.Linear)]
    [InlineData(McapBufferReadMode.SansMagic)]
    [InlineData(McapBufferReadMode.FlattenChunks)]
    [InlineData(McapBufferReadMode.Chunk)]
    public void RecordOnlyModeRejectsAllMessageDeliveryWithoutAdvancing(McapBufferReadMode mode)
    {
        var input = Recording();
        if (mode == McapBufferReadMode.SansMagic) input = input[8..^8];
        if (mode == McapBufferReadMode.Chunk)
        {
            using var top = new McapBufferReader(input, McapBufferReadMode.Linear);
            input = top.ReadRecords().First(r => r.Opcode == 6).Data;
        }
        using var reader = new McapBufferReader(input, mode);
        AssertMessageMethodsRejected(reader);
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord([], out var opcode, out var length));
        AssertMessageMethodsRejected(reader);
        Assert.Equal(McapReadStatus.Success, reader.ReadNextRecord(new byte[checked((int)length)], out var retryOpcode, out _));
        Assert.Equal(opcode, retryOpcode);
        Assert.Equal(mode == McapBufferReadMode.Chunk ? 3 : 1, opcode);
    }

    static void AssertMessageMethodsRejected(McapBufferReader reader)
    {
        Assert.Throws<InvalidOperationException>(() => reader.ReadNext(new byte[100], out _, out _));
        Assert.Throws<InvalidOperationException>(() => reader.ReadNext((in McapMessageHeader _, ReadOnlySpan<byte> _) => true));
        Assert.Throws<InvalidOperationException>(() => reader.ReadBatch(new McapMessageHeader[1], new McapPayloadRange[1], new byte[100]));
        Assert.Throws<InvalidOperationException>(() => reader.ReadBatchLease());
        Assert.Throws<InvalidOperationException>(() => reader.ReadMessages().ToArray());
    }

    [Fact]
    public void InternalSummaryAndChunkCursorsHaveDistinctCapabilities()
    {
        using var stream = new MemoryStream(Recording());
        using var reader = McapFileReader.OpenRecords(stream);
        using var snapshot = reader.OpenIndexSnapshot();
        using var summary = snapshot.OpenSummaryRecords();
        AssertMessageMethodsRejected(summary);
        Assert.Equal(McapReadStatus.Success, summary.ReadNextRecord(new byte[4096], out _, out _));
        using var chunk = snapshot.OpenChunkReader(Assert.Single(reader.GetSummary()!.ChunkIndexes));
        Assert.Equal(McapReadStatus.BufferTooSmall, chunk.ReadNext([], out _, out var length));
        Assert.Equal(3ul, length);
        Assert.Equal(McapReadStatus.Success, chunk.ReadNext(new byte[3], out var header, out _));
        Assert.Equal((ushort)11, header.ChannelId);
    }

    [Fact]
    public void RecoveryRetainsStructuredParseError()
    {
        using var stream = new MemoryStream(Recording(complete: false));
        using var reader = McapFileReader.OpenMessages(stream, options: McapReaderOptions.Strict);
        var result = reader.RecoverMessages(_ => { });
        Assert.Equal(1ul, result.RecoveredMessageCount);
        Assert.False(result.IsFullyValidated);
        Assert.NotNull(result.Error);
        Assert.Equal(McapErrorKind.UnexpectedEof, result.Error.Kind);
        Assert.False(string.IsNullOrEmpty(result.Error.Message));
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public async Task AsyncLeaseDeclarationsAreImmutableCachedAndRequireConsumedOperation(McapCompression compression)
    {
        using var stream = new SuspendedStream(Recording(compression));
        using var reader = new McapAsyncReader(stream, inputBufferSize: 7);
        Assert.Throws<McapException>(() => reader.GetChannelDescription(11));
        Assert.Throws<McapException>(() => reader.GetSchemaDescription(7));
        var operation = reader.ReadBatchLeaseAsync(1);
        Assert.Throws<InvalidOperationException>(() => reader.GetChannelDescription(11));
        Assert.Throws<InvalidOperationException>(() => reader.GetSchemaDescription(7));
        using var batch = await operation;
        Assert.NotNull(batch);
        var channel = reader.GetChannelDescription(batch.GetHeader(0).ChannelId);
        var schema = reader.GetSchemaDescription(7);
        Assert.Equal("topic", channel.Topic);
        Assert.Equal("raw", channel.MessageEncoding);
        Assert.Same(schema, channel.Schema);
        Assert.Equal("unused", reader.GetSchemaDescription(8).Name);
        Assert.Same(channel, reader.GetChannelDescription(11));
        Assert.Throws<McapException>(() => reader.GetChannelDescription(99));
        Assert.Throws<McapException>(() => reader.GetSchemaDescription(99));
        Assert.Null(await reader.ReadBatchLeaseAsync(1));
        Assert.Same(schema, reader.GetSchemaDescription(7));
        reader.Dispose();
        Assert.Throws<ObjectDisposedException>(() => reader.GetChannelDescription(11));
        Assert.Equal(new byte[] { 9, 8 }, schema.Data.ToArray());
        Assert.Equal(new byte[] { 1, 2, 3 }, batch.GetPayload(0).ToArray());
    }

    [Fact]
    public async Task AsyncRecordConsumptionDoesNotOfferLeaseDeclarations()
    {
        using var stream = new MemoryStream(Recording());
        using var reader = new McapAsyncReader(stream);
        await reader.ReadNextRecordAsync(new byte[4096]);
        Assert.Throws<InvalidOperationException>(() => reader.GetChannelDescription(11));
        Assert.Throws<InvalidOperationException>(() => reader.GetSchemaDescription(7));
        Assert.Equal(McapReadStatus.Success, (await reader.ReadNextRecordAsync(new byte[4096])).Status);
    }

    [Fact]
    public async Task AsyncDeclarationLookupRejectsTerminalReaderEvenWithCachedResult()
    {
        using var stream = new MemoryStream(Recording());
        using var reader = new McapAsyncReader(stream);
        using var batch = await reader.ReadBatchLeaseAsync(1);
        var declaration = reader.GetChannelDescription(11);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () =>
            await reader.ReadBatchLeaseAsync(cancellationToken: cancellation.Token));
        Assert.Throws<InvalidOperationException>(() => reader.GetChannelDescription(11));
        Assert.Throws<InvalidOperationException>(() => reader.GetSchemaDescription(7));
        Assert.Equal("topic", declaration.Topic);
    }

    sealed class SuspendedStream(byte[] bytes) : MemoryStream(bytes)
    {
        public override bool CanSeek => false;
        public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
        {
            await Task.Yield();
            return await base.ReadAsync(buffer, cancellationToken);
        }
    }
}
