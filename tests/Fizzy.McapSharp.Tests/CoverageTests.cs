using Xunit;

namespace Fizzy.McapSharp.Tests;

public class CoverageTests
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void RawMessagesRetainsUnusedChannelsIncludingLateDeclarations(McapCompression compression)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, ChunkSize = 1, EmitSummaryRecords = false }, true))
        {
            writer.RegisterSchema(7, "schema", "raw", [42]);
            writer.RegisterChannel(0, "unused-before", "raw", 7);
            writer.RegisterChannel(1, "used", "raw");
            writer.WriteMessage(new McapMessageHeader(1, 0, 1, 1), [1]);
            writer.RegisterChannel(ushort.MaxValue, "unused-after", "raw", 7);
            writer.Complete();
        }
        using var reader = new McapBufferReader(stream.ToArray(), McapBufferReadMode.RawMessages);
        Assert.Single(reader.ReadMessages());
        Assert.Equal("unused-before", reader.GetChannel(0).Topic);
        Assert.Equal("unused-after", reader.GetChannel(ushort.MaxValue).Topic);
        Assert.Equal(new byte[] { 42 }, reader.GetChannel(ushort.MaxValue).Schema!.Data);
        Assert.Equal(McapErrorKind.UnknownChannel, Assert.Throws<McapException>(() => reader.GetChannel(2)).Kind);
    }

    [Fact]
    public void RawMessagesRetainsChannelsInRecordingWithoutMessages()
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, leaveOpen: true))
        { writer.RegisterChannel(7, "unused", "raw"); writer.Complete(); }
        using var reader = new McapBufferReader(stream.ToArray(), McapBufferReadMode.RawMessages);
        Assert.Empty(reader.ReadMessages());
        Assert.Equal("unused", reader.GetChannel(7).Topic);
    }

    [Fact]
    public void SnapshotUsesCallerMetadataAndAttachmentIndexesWithoutSummary()
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { EmitSummaryRecords = false, EmitSummaryOffsets = false }, true))
        {
            writer.WriteMetadata("元数据", new Dictionary<string, string> { ["k"] = "v" });
            writer.WriteAttachment("附件", "application/测试", 1, 2, [3, 4]);
            writer.Complete();
        }
        var bytes = stream.ToArray();
        using var records = new McapBufferReader(bytes, McapBufferReadMode.Linear);
        McapMetadataIndex metadata = null!;
        McapAttachmentIndex attachment = null!;
        ulong offset = 8;
        foreach (var record in records.ReadRecords())
        {
            ulong length = (ulong)record.Data.Length + 9;
            if (record.Opcode == 12) metadata = new(offset, length, "caller metadata");
            if (record.Opcode == 9) attachment = new(offset, length, 10, 20, 30, "caller attachment", "caller type");
            offset += length;
        }
        using var snapshot = new McapIndexSnapshot(bytes);
        Assert.Null(snapshot.GetSummary());
        Assert.Equal("元数据", snapshot.ReadMetadata(metadata).Name);
        Assert.Equal(new byte[] { 3, 4 }, snapshot.ReadAttachment(attachment).Data);
        var buffer = Enumerable.Repeat((byte)0xCC, 256).ToArray();
        Assert.Equal(McapReadStatus.BufferTooSmall, snapshot.ReadMetadata(metadata, buffer.AsSpan(0, 1), out var required));
        Assert.All(buffer, b => Assert.Equal((byte)0xCC, b));
        Assert.Equal(McapReadStatus.Message, snapshot.ReadMetadata(metadata, buffer, out var copied));
        Assert.Equal(required, copied);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadMetadata(metadata with { Length = 0 })).Kind);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadAttachment(attachment with { Length = 0 })).Kind);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadMetadata(metadata with { Offset = ulong.MaxValue })).Kind);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadAttachment(attachment with { Length = ulong.MaxValue })).Kind);
        Assert.Equal("元数据", snapshot.ReadMetadata(metadata).Name);
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void SnapshotUsesCallerChunkFieldsAndCurrentOffsetMap(McapCompression compression)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression }, true))
        {
            writer.RegisterChannel(1, "one", "raw"); writer.RegisterChannel(2, "two", "raw");
            writer.WriteMessage(new McapMessageHeader(1, 10, 1, 1), [1]);
            writer.WriteMessage(new McapMessageHeader(2, 20, 2, 2), [2]);
            writer.Complete();
        }
        using var snapshot = new McapIndexSnapshot(stream.ToArray());
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        var offsets = new Dictionary<ushort, ulong> { [1] = chunk.MessageIndexOffsets[1] };
        var selected = chunk with { MessageIndexOffsets = new System.Collections.ObjectModel.ReadOnlyDictionary<ushort, ulong>(offsets) };
        var entries = snapshot.ReadMessageIndexes(selected);
        Assert.Equal((ushort)1, Assert.Single(entries).ChannelId);
        offsets.Clear(); offsets.Add(2, chunk.MessageIndexOffsets[2]);
        Assert.Equal((ushort)2, Assert.Single(snapshot.ReadMessageIndexes(selected)).ChannelId);
        offsets.Clear();
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadMessageIndexes(selected)).Kind);
        offsets.Add(1, ulong.MaxValue);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.ReadMessageIndexes(selected)).Kind);
        // Upstream message-index reads use the offset map, not the chunk's own offset.
        Assert.Equal(2, snapshot.ReadMessageIndexes(chunk with { ChunkStartOffset = ulong.MaxValue }).Count);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.OpenChunkMessages(chunk with { ChunkLength = ulong.MaxValue })).Kind);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.SeekMessage(chunk with { ChunkLength = ulong.MaxValue }, entries[0].Records[0])).Kind);
        var changed = chunk with { ChunkStartOffset = 100, Compression = "测试" };
        Assert.Equal(McapRecords.GetCompressedDataOffset(100, System.Text.Encoding.UTF8.GetBytes("测试")), snapshot.GetCompressedDataOffset(changed));
        var buffer = new byte[] { 0xCC };
        Assert.Equal(McapReadStatus.BufferTooSmall, snapshot.SeekMessage(chunk, entries[0].Records[0], [], out var header, out var length));
        Assert.Equal(1ul, length);
        Assert.Equal(McapReadStatus.Message, snapshot.SeekMessage(chunk, entries[0].Records[0], buffer, out var retried, out _));
        Assert.Equal(header, retried); Assert.Equal((byte)1, buffer[0]);
        Assert.Equal(2, snapshot.ReadChunkMessages(chunk).Count());
    }

    static McapChannel Channel(ushort id = 7, string topic = "topic") => new(id, topic, "raw", new(9, "schema", "raw", [1, 2]), new Dictionary<string, string>());
    static byte[] Recording(bool chunks = true)
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, new() { UseChunks = chunks, ChunkSize = 64, CompressionThreads = 0 }, true))
        {
            using var c = new McapPreparedChannel(Channel());
            foreach (var t in new ulong[] { 3, 1, 2, 1 }) w.WriteMessage(c, new(7, (uint)t, t, t), [(byte)t]);
            w.WriteMetadata("metadata", new Dictionary<string, string> { ["a"] = "b" });
            w.WriteAttachment("attachment", "raw", 4, 5, [8, 9]);
            w.Complete();
        }
        return s.ToArray();
    }
    [Fact]
    public void DefaultsMatchRustAndCompleteMessageDeclaresChannel()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, leaveOpen: true))
        {
            w.WriteMessage(new McapMessage(Channel(), 1, 2, 3, [4]));
            Assert.Equal(1ul, w.Finish().Statistics!.MessageCount);
        }
        Assert.Equal(1024ul * 1024, new McapWriterOptions().ChunkSize);
        s.Position = 0;
        using var r = McapReader.OpenRecords(s, McapRecordMode.TopLevel, true);
        var records = r.ReadRecords().ToArray();
        Assert.Equal(McapFormat.LibraryIdentifier, ((McapHeader)McapRecords.Parse(1, records[0].Data)).Library);
        Assert.Equal("zstd", ((McapChunkRecord)McapRecords.Parse(6, records.First(x => x.Opcode == 6).Data)).Header.Compression);
        Assert.True(r.IsScanComplete); Assert.False(r.IsComplete);
    }
    [Theory]
    [InlineData(true)] [InlineData(false)]
    public void SortingAndMultiTopicHaveSameSemanticsWithAndWithoutIndexes(bool chunks)
    {
        var bytes = Recording(chunks);
        foreach (var order in Enum.GetValues<McapReadOrder>())
        {
            using var r = McapReader.OpenMessages(new MemoryStream(bytes), new() { Topics = ["topic", "missing"], Order = order });
            var times = r.ReadMessages().Select(m => m.LogTime).ToArray();
            Assert.Equal(order == McapReadOrder.File ? new ulong[] { 3, 1, 2, 1 } : order == McapReadOrder.LogTime ? [1ul, 1, 2, 3] : [3ul, 2, 1, 1], times);
        }
        using var empty = McapReader.OpenMessages(new MemoryStream(bytes), new() { Topics = ["missing"] });
        Assert.Empty(empty.ReadMessages());
    }
    [Fact]
    public void PreparedDescriptorsSnapshotMutableInputs()
    {
        var c = Channel();
        using var prepared = new McapPreparedChannel(c);
        c.Schema!.Data[0] = 99;
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, leaveOpen: true))
        { w.WriteMessage(prepared, new(7, 0, 1, 2), [3]); w.Complete(); }
        s.Position = 0;
        using var r = McapReader.OpenMessages(s);
        Assert.Equal((byte)1, r.ReadMessages().Single().Channel.Schema!.Data[0]);
    }
    [Fact]
    public void RandomUpstreamOperationsAndRetryPreserveCursor()
    {
        using var r = McapReader.OpenMessages(new MemoryStream(Recording()));
        var summary = r.GetSummary()!;
        using var snapshot = r.OpenIndexSnapshot();
        var chunk = summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0);
        var indexes = r.ReadMessageIndexes(chunk);
        Assert.Equal(McapReadStatus.BufferTooSmall, snapshot.SeekMessage(chunk, indexes[0].Records[0], [], out var h, out var n));
        Assert.Equal(1ul, n);
        Span<byte> b = stackalloc byte[128];
        Assert.Equal(McapReadStatus.Message, snapshot.SeekMessage(chunk, indexes[0].Records[0], b, out var retry, out _));
        Assert.Equal(h, retry);
        snapshot.OpenChunkMessages(chunk);
        Assert.Equal(McapReadStatus.Message, snapshot.ReadNext(b, out _, out _));
        Assert.Equal("metadata", snapshot.ReadMetadata(summary.MetadataIndexes[0]).Name);
        Assert.Equal(new byte[] { 8, 9 }, snapshot.ReadAttachment(summary.AttachmentIndexes[0]).Data);
        Assert.True(snapshot.ReadFooter().SummaryStart > 0);
        Assert.Equal(McapReadStatus.Message, r.ReadNext(b, out var original, out _));
        Assert.Equal(3ul, original.LogTime);
    }
    [Fact]
    public void SansIoSummaryAndIndexedEventsUseUpstream()
    {
        using var file = new MemoryStream(Recording());
        using var s = McapSansIoReader.CreateSummary(new() { FileSize = (ulong)file.Length });
        byte[] buffer = new byte[65536];
        while (s.NextEvent(buffer, out var e) != McapReadStatus.EndOfStream)
        {
            if (e.Kind == McapReadEventKind.Seek) s.NotifySeeked((ulong)file.Seek(unchecked((long)e.Offset), e.Origin));
            else { int n = file.Read(buffer, 0, (int)Math.Min((ulong)buffer.Length, e.Length)); s.SupplyInput(buffer.AsSpan(0, n)); }
        }
        Assert.Equal(4ul, s.GetSummary()!.Statistics!.MessageCount);
        using var indexed = s.CreateIndexed();
        var times = new List<ulong>();
        while (indexed.NextEvent(buffer, out var e) != McapReadStatus.EndOfStream)
        {
            if (e.Kind == McapReadEventKind.ReadChunk) { file.Position = (long)e.Offset; file.ReadExactly(buffer.AsSpan(0, (int)e.Length)); indexed.SupplyInput(buffer.AsSpan(0, (int)e.Length), e.Offset); }
            else times.Add(e.Header.LogTime);
        }
        Assert.Equal(new ulong[] { 1, 1, 2, 3 }, times);
    }
    [Fact]
    public async Task AsyncRecordsRetryOwnershipAndCancellation()
    {
        using var stream = new MemoryStream(Recording());
        using (var r = new McapAsyncReader(stream, leaveOpen: true, inputBufferSize: 3))
        {
            var small = await r.ReadNextRecordAsync(Memory<byte>.Empty);
            Assert.Equal(McapReadStatus.BufferTooSmall, small.Status);
            var data = new byte[checked((int)small.Length)];
            Assert.Equal(McapReadStatus.Message, (await r.ReadNextRecordAsync(data)).Status);
            using var cancelled = new CancellationTokenSource(); cancelled.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await r.ReadNextRecordAsync(data, cancelled.Token));
            Assert.Throws<InvalidOperationException>(() => r.ReadNextRecordAsync(data));
        }
        Assert.True(stream.CanRead);
    }
    [Fact]
    public void DefaultsDoNotClaimFullValidationAndStrictDoes()
    {
        using var normal = McapReader.OpenMessages(new MemoryStream(Recording()));
        Assert.Equal(4, normal.ReadMessages().Count()); Assert.True(normal.IsScanComplete); Assert.False(normal.IsComplete);
        using var strict = McapReader.OpenMessages(new MemoryStream(Recording()), options: McapReaderOptions.Strict);
        Assert.True(strict.ValidateRemaining() > 0); Assert.True(strict.IsComplete);
    }
    [Fact]
    public void LengthLimitsAndStructuredErrors()
    {
        using var r = McapReader.OpenRecords(new MemoryStream(Recording()), options: new() { RecordLengthLimit = 1 });
        var error = Assert.Throws<McapException>(() => r.ReadNextRecord([], out _, out _));
        Assert.Equal(McapErrorKind.RecordTooLarge, error.Kind);
        Assert.Equal(1, error.Details.GetProperty("opcode").GetInt32());
    }
    [Fact]
    public void TransferDoesNotCompleteOrCloseStream()
    {
        using var s = new MemoryStream();
        var w = new McapWriter(s, new() { UseChunks = false });
        w.RegisterChannel("t", "raw");
        Assert.Same(s, w.IntoInner()); Assert.True(s.CanWrite);
        Assert.Throws<ObjectDisposedException>(() => w.Complete());
    }
    [Theory]
    [InlineData(McapBufferReadMode.Linear)]
    [InlineData(McapBufferReadMode.FlattenChunks)]
    [InlineData(McapBufferReadMode.RawMessages)]
    [InlineData(McapBufferReadMode.Messages)]
    public void DirectSliceReadersAndRecordModels(McapBufferReadMode mode)
    {
        var bytes = Recording();
        using var reader = new McapBufferReader(bytes, mode);
        var records = reader.ReadRecords().ToArray();
        Assert.NotEmpty(records);
        Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNextRecord([], out var endOpcode, out var endLength));
        Assert.Equal((byte)0, endOpcode); Assert.Equal(0ul, endLength);
        foreach (var record in records) Assert.NotNull(McapRecords.Parse(record.Opcode, record.Data));
        if (mode is McapBufferReadMode.Messages or McapBufferReadMode.RawMessages)
        { Assert.Equal(4, records.Length); Assert.Equal("topic", reader.GetChannel(7).Topic); }
        if (mode == McapBufferReadMode.Linear)
        {
            using var chunk = new McapBufferReader(records.First(r => r.Opcode == 6).Data, McapBufferReadMode.Chunk);
            Assert.NotEmpty(chunk.ReadRecords());
        }
        using var snapshot = new McapIndexSnapshot(bytes);
        using var summary = snapshot.OpenSummaryRecords();
        Assert.Contains(summary.ReadRecords(), r => r.Opcode == 11);
        Assert.Equal(snapshot.ReadFooter(), McapRecords.ReadFooter(bytes));
    }
    [Fact]
    public void WriterOptionPrecedenceAndSummaryCursor()
    {
        using var s = new MemoryStream();
        using var w = new McapWriter(s, new() { EmitSummaryRecords = false, EmitStatistics = true, DisableSeeking = true, CompressionThreads = 0, ChunkSize = 0 }, true);
        w.WriteMessage(new McapMessage(Channel(), 1, 1, 1, []));
        w.Complete();
        using (var summary = w.OpenSummaryRecords()) Assert.Contains(summary.ReadRecords(), r => r.Opcode == 11);
        w.Dispose();
        s.Position = 0;
        using var r = McapReader.OpenRecords(s, leaveOpen: true);
        var snapshot = r.GetSummary()!;
        Assert.NotNull(snapshot.Statistics); Assert.Empty(snapshot.ChunkIndexes); Assert.Empty(snapshot.ChannelIds);
    }
    [Fact]
    public void UnknownRecordAndCompressedOffset()
    {
        var record = (McapRecord)McapRecords.Parse(0x81, [1, 2]);
        Assert.Equal(new byte[] { 1, 2 }, record.Data);
        Assert.Equal(153ul, McapRecords.GetCompressedDataOffset(100, "zstd"u8));
        var e = Assert.Throws<McapException>(() => McapRecords.GetCompressedDataOffset(ulong.MaxValue, "zstd"u8));
        Assert.Equal(McapErrorKind.BadChunkStartOffset, e.Kind);
    }
    [Fact]
    public void SansMagicAndIgnoreEndMagicAreDirectSliceOptions()
    {
        var bytes = Recording(false);
        using var noEnd = new McapBufferReader(bytes.AsSpan(0, bytes.Length - 8), McapBufferReadMode.Messages, true);
        Assert.Equal(4, noEnd.ReadMessages().Count());
        using var noMagic = new McapBufferReader(bytes.AsSpan(8, bytes.Length - 16), McapBufferReadMode.SansMagic);
        Assert.Contains(noMagic.ReadRecords(), r => r.Opcode == 5);
    }
    [Fact]
    public async Task SuspendedCancellationRejectsConcurrentOperations()
    {
        using var stream = new PendingStream();
        using var reader = new McapAsyncReader(stream, leaveOpen: true);
        using var cancellation = new CancellationTokenSource();
        var pending = reader.ReadNextRecordAsync(new byte[256], cancellation.Token);
        Assert.False(pending.IsCompleted);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[256]));
        Assert.Throws<InvalidOperationException>(() => reader.Dispose());
        Assert.Throws<InvalidOperationException>(() => McapReader.OpenMessages(stream));
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await pending);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[256]));
    }
    sealed class PendingStream : Stream
    {
        readonly TaskCompletionSource<int> source = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken token = default) => new(source.Task.WaitAsync(token));
        public override bool CanRead => true;
        public override bool CanWrite => false;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override void Flush() => throw new NotSupportedException();
        public override int Read(byte[] b, int o, int n) => throw new NotSupportedException();
        public override long Seek(long o, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long n) => throw new NotSupportedException();
        public override void Write(byte[] b, int o, int n) => throw new NotSupportedException();
    }
}
