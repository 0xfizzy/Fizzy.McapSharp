using System.Buffers.Binary;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ValidationContractTests
{
    static byte[] Recording(McapCompression compression = McapCompression.None, bool chunks = false)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new()
        {
            Compression = compression, CompressionThreads = 0, UseChunks = chunks,
            CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            writer.WriteMessage(new(channel, 7, 10, 10), [42]);
            writer.WriteAttachment("attachment", "raw", 10, 10, [10, 20]);
            writer.Complete();
        }
        return stream.ToArray();
    }

    static IEnumerable<int> Records(byte[] bytes, byte opcode)
    {
        for (int p = 8; p < bytes.Length - 8; p += 9 + checked((int)BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(p + 1))))
            if (bytes[p] == opcode) yield return p;
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public async Task StrictAsyncRejectsAttachmentCrcAfterBufferRetryAndShortReads(McapCompression compression)
    {
        var data = Recording(compression, chunks: true);
        int attachment = Records(data, 9).Single();
        int size = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(attachment + 1)));
        data[attachment + 9 + size - 1] ^= 1;
        using var reader = new McapAsyncReader(new MemoryStream(data), McapReaderOptions.Strict, inputBufferSize: 3);
        byte[] buffer = [];
        var error = await Assert.ThrowsAsync<McapException>(async () =>
        {
            while (true)
            {
                var result = await reader.ReadNextRecordAsync(buffer);
                if (result.Status == McapReadStatus.BufferTooSmall) buffer = new byte[checked((int)result.Length)];
                else if (result.Status == McapReadStatus.EndOfStream) break;
            }
        });
        Assert.Equal(McapErrorKind.BadAttachmentCrc, error.Kind);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(buffer));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void ExpandedSessionsRejectUnknownSchemaRegardlessOfDelivery(bool messages)
    {
        var data = Recording();
        foreach (var offset in Records(data, 4))
            BinaryPrimitives.WriteUInt16LittleEndian(data.AsSpan(offset + 11), 123);
        using var reader = messages
            ? McapReaderFactory.OpenMessages(new MemoryStream(data), options: McapReaderOptions.Strict)
            : McapReaderFactory.OpenRecords(new MemoryStream(data), options: McapReaderOptions.Strict);
        var error = Assert.Throws<McapException>(() => reader.ValidateRemaining());
        Assert.Equal(McapErrorKind.UnknownSchema, error.Kind);
        Assert.False(reader.IsFullyValidated);
    }

    [Fact]
    public void FactoryValidationChecksDeclarationReferences()
    {
        var data = Recording();
        foreach (var offset in Records(data, 4))
            BinaryPrimitives.WriteUInt16LittleEndian(data.AsSpan(offset + 11), 123);
        var path = Path.Combine(Path.GetTempPath(), $"mcap-validation-{Guid.NewGuid():N}.mcap");
        try
        {
            File.WriteAllBytes(path, data);
            Assert.Equal(McapErrorKind.UnknownSchema, Assert.Throws<McapException>(() => new McapReaderFactory(path).Validate()).Kind);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void UnknownMessageChannelHasStructuredErrorInBothSessionModes(bool messages)
    {
        var data = Recording();
        BinaryPrimitives.WriteUInt16LittleEndian(data.AsSpan(Records(data, 5).Single() + 9), 123);
        using var reader = messages
            ? McapReaderFactory.OpenMessages(new MemoryStream(data), options: McapReaderOptions.Strict)
            : McapReaderFactory.OpenRecords(new MemoryStream(data), options: McapReaderOptions.Strict);
        Assert.Equal(McapErrorKind.UnknownChannel, Assert.Throws<McapException>(() => reader.ValidateRemaining()).Kind);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void SummaryLookupDoesNotSupplyMissingDataDeclarations(bool removeSchema)
    {
        using var source = new MemoryStream();
        using (var writer = new McapWriter(source, new()
        {
            UseChunks = false, CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, leaveOpen: true))
        {
            var schema = writer.RegisterSchema("schema", "raw", [1]);
            var channel = writer.RegisterChannel("topic", "raw", schema);
            writer.WriteMessage(new(channel, 0, 1, 1), [42]);
            writer.Complete();
        }
        var data = source.ToArray();
        data[Records(data, removeSchema ? (byte)3 : (byte)4).First()] = 0x80;
        using var reader = McapReaderFactory.OpenRecords(new MemoryStream(data), options: McapReaderOptions.Strict);
        Assert.NotNull(reader.GetSummary());
        var error = Assert.Throws<McapException>(() => reader.ValidateRemaining());
        Assert.Equal(removeSchema ? McapErrorKind.UnknownSchema : McapErrorKind.UnknownChannel, error.Kind);
        Assert.False(reader.IsFullyValidated);
    }

    [Fact]
    public void SummaryFallbackDoesNotAdvanceSequentialValidationState()
    {
        using var source = new MemoryStream();
        using (var writer = new McapWriter(source, new()
        {
            UseChunks = false, RepeatSchemas = false,
            CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, leaveOpen: true))
        {
            var schema = writer.RegisterSchema("schema", "raw", [1]);
            var channel = writer.RegisterChannel("topic", "raw", schema);
            writer.WriteMessage(new(channel, 0, 1, 1), [42]);
            writer.Complete();
        }
        var data = source.ToArray();
        using var reader = McapReaderFactory.OpenRecords(new MemoryStream(data), options: McapReaderOptions.Strict);
        Assert.NotNull(reader.GetSummary());
        Assert.Equal(0ul, reader.ScannedRecordCount);
        Assert.False(reader.IsScanComplete);
        Assert.Equal(McapReadStatus.Success, reader.ReadNextRecord(new byte[4096], out var opcode, out _));
        Assert.Equal((byte)1, opcode);
        Assert.True(reader.ValidateRemaining() > 0);
        Assert.True(reader.IsFullyValidated);
    }

    [Fact]
    public void RecoveryRequiresStrictOptionsWithoutAdvancingSession()
    {
        using var reader = McapReaderFactory.OpenMessages(new MemoryStream(Recording()));
        Assert.Throws<InvalidOperationException>(() => reader.RecoverMessages(_ => { }));
        Assert.Equal(McapReadStatus.Success, reader.ReadNext(new byte[1], out _, out _));
    }

    [Fact]
    public void RecoveryPropagatesStreamMcapExceptionByIdentity()
    {
        var expected = new McapException("stream-origin");
        using var reader = McapReaderFactory.OpenMessages(new ThrowingStream(expected), options: McapReaderOptions.Strict);
        Assert.Same(expected, Assert.Throws<McapException>(() => reader.RecoverMessages(_ => { })));
        Assert.Throws<InvalidOperationException>(() => reader.ReadNext(new byte[1], out _, out _));
    }

    [Fact]
    public void StrictRecoveryRejectsBadChunkBeforeDelivery()
    {
        var data = Recording(chunks: true);
        data[Records(data, 6).First() + 9 + 24] ^= 1;
        using var reader = McapReaderFactory.OpenMessages(new MemoryStream(data), options: McapReaderOptions.Strict);
        var result = reader.RecoverMessages(_ => throw new InvalidOperationException("Invalid chunk was delivered."));
        Assert.Equal(0ul, result.RecoveredMessageCount);
        Assert.Equal(McapErrorKind.BadChunkCrc, result.Error!.Kind);
    }

    sealed class ThrowingStream(McapException error) : MemoryStream
    {
        public override int Read(Span<byte> buffer) => throw error;
    }
}
