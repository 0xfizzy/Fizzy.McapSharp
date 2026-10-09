using Xunit;

namespace Fizzy.McapSharp.Tests;

public partial class CoverageTests
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
        using var reader = new McapReadCursor(stream.ToArray(), McapCursorMode.RawMessages);
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
        using var reader = new McapReadCursor(stream.ToArray(), McapCursorMode.RawMessages);
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
        using var records = new McapReadCursor(bytes, McapCursorMode.TopLevelRecords);
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
        Assert.Equal(McapReadStatus.Success, snapshot.ReadMetadata(metadata, buffer, out var copied));
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
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.OpenChunkReader(chunk with { ChunkLength = ulong.MaxValue })).Kind);
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() => snapshot.SeekMessage(chunk with { ChunkLength = ulong.MaxValue }, entries[0].Records[0])).Kind);
        var changed = chunk with { ChunkStartOffset = 100, Compression = "测试" };
        Assert.Equal(McapRecords.GetCompressedDataOffset(100, System.Text.Encoding.UTF8.GetBytes("测试")), snapshot.GetCompressedDataOffset(changed));
        var buffer = new byte[] { 0xCC };
        Assert.Equal(McapReadStatus.BufferTooSmall, snapshot.SeekMessage(chunk, entries[0].Records[0], [], out var header, out var length));
        Assert.Equal(1ul, length);
        Assert.Equal(McapReadStatus.Success, snapshot.SeekMessage(chunk, entries[0].Records[0], buffer, out var retried, out _));
        Assert.Equal(header, retried); Assert.Equal((byte)1, buffer[0]);
        Assert.Equal(2, snapshot.ReadChunkMessages(chunk).Count());
    }

}
