using System.Buffers.Binary;
using Xunit;
namespace Fizzy.McapSharp.Tests;

public class LazyReaderTests
{
    static byte[] Recording(bool chunks, McapCompression compression = McapCompression.None)
    {
        using var stream = new MemoryStream();
        using (var w = new McapWriter(stream, new() { UseChunks = chunks, ChunkSize = 1, Compression = compression }, true))
        {
            var channel = w.RegisterChannel("topic", "raw");
            uint sequence = 0;
            foreach (ulong time in new ulong[] { 3, 1, 2, 1 })
                w.WriteMessage(new McapMessageHeader(channel, sequence++, time, time), new byte[] { (byte)sequence });
            w.Complete();
        }
        return stream.ToArray();
    }
    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void IndependentChunkCursorsSurviveSnapshotAndPreserveRetries(McapCompression compression)
    {
        var snapshot = new McapIndexSnapshot(Recording(true, compression));
        var chunks = snapshot.GetSummary()!.ChunkIndexes.Where(c => c.MessageIndexOffsets.Count > 0).ToArray();
        using var first = snapshot.OpenChunkReader(chunks[0]);
        using var second = snapshot.OpenChunkReader(chunks[1]);
        Assert.Equal(McapReadStatus.BufferTooSmall, first.ReadNext([], out var pending, out var length));
        Assert.Throws<McapException>(() => snapshot.OpenChunkReader(chunks[0] with { ChunkLength = ulong.MaxValue }));

        snapshot.ReadFooter();
        snapshot.Dispose();
        byte[] buffer = new byte[16];
        Assert.Equal(McapReadStatus.Message, second.ReadNext(buffer, out var other, out _));
        Assert.NotEqual(pending.Sequence, other.Sequence);
        Assert.Equal(McapReadStatus.Message, first.ReadNext(buffer, out var retry, out var copied));
        Assert.Equal(pending, retry); Assert.Equal(length, copied);
        Assert.Equal(McapReadStatus.EndOfStream, first.ReadNext(buffer, out _, out _));
        Assert.Equal(McapReadStatus.EndOfStream, first.ReadNext(buffer, out _, out _));
    }
    [Fact]
    public void ConvenienceChunkEnumeratorsHaveIndependentCursors()
    {
        using var snapshot = new McapIndexSnapshot(Recording(true));
        var chunks = snapshot.GetSummary()!.ChunkIndexes.Where(c => c.MessageIndexOffsets.Count > 0).ToArray();
        using var cursor = snapshot.OpenChunkReader(chunks[2]);
        using var a = snapshot.ReadChunkMessages(chunks[0]).GetEnumerator();
        using var b = snapshot.ReadChunkMessages(chunks[1]).GetEnumerator();
        Assert.True(a.MoveNext()); Assert.True(b.MoveNext());
        Assert.Equal(0u, a.Current.Sequence); Assert.Equal(1u, b.Current.Sequence);
        Assert.Equal(McapReadStatus.Message, cursor.ReadNext(new byte[16], out var h, out _));
        Assert.Equal(2u, h.Sequence);
        Assert.False(a.MoveNext()); Assert.False(b.MoveNext());
    }
    [Fact]
    public void SliceDescriptionsAppearOnlyAfterAdvancementAndErrorsAreDeferred()
    {
        var data = Recording(false);
        // Damage the second message opcode/length framing, after a valid first message.
        int offset = 8, messages = 0;
        while (offset < data.Length - 8)
        {
            int length = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset + 1)));
            if (data[offset] == 5 && ++messages == 2) { BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(offset + 1), ulong.MaxValue); break; }
            offset += length + 9;
        }
        using var r = new McapBufferReader(data);
        Assert.Equal(McapErrorKind.UnknownChannel, Assert.Throws<McapException>(() => r.GetChannel(1)).Kind);
        byte[] output = new byte[16];
        Assert.Equal(McapReadStatus.Message, r.ReadNext(output, out var first, out _));
        Assert.Equal("topic", r.GetChannel(first.ChannelId).Topic);
        Assert.Throws<McapException>(() => r.ReadNext(output, out _, out _));
        Assert.Equal("topic", r.GetChannel(first.ChannelId).Topic);
        Assert.Throws<McapException>(() => r.ReadNext(output, out _, out _));
    }
    [Theory]
    [InlineData(true)] [InlineData(false)]
    public void QueryOrderingUsesSequenceForTiesAndBufferedSortCanBeDisabled(bool chunks)
    {
        byte[] bytes = Recording(chunks);
        foreach (var order in Enum.GetValues<McapReadOrder>())
        {
            var query = new McapQuery { Order = order, AllowBufferedSort = false };
            if (!chunks && order != McapReadOrder.File)
                Assert.Throws<NotSupportedException>(() => McapReader.OpenMessages(new MemoryStream(bytes), query));
            else
            {
                using var reader = McapReader.OpenMessages(new MemoryStream(bytes), query);
                Assert.Equal(order == McapReadOrder.File ? new uint[] { 0, 1, 2, 3 } : order == McapReadOrder.LogTime ? [1u, 3, 2, 0] : [0u, 2, 3, 1], reader.ReadMessages().Select(m => m.Sequence));
            }
            using var fallback = McapReader.OpenMessages(new NonSeekable(bytes), query with { AllowBufferedSort = true });
            Assert.Equal(order == McapReadOrder.File ? new uint[] { 0, 1, 2, 3 } : order == McapReadOrder.LogTime ? [1u, 3, 2, 0] : [0u, 2, 3, 1], fallback.ReadMessages().Select(m => m.Sequence));
        }
        Assert.Throws<NotSupportedException>(() => McapReader.OpenMessages(new NonSeekable(bytes), new() { AllowBufferedSort = false }));
        Assert.Throws<NotSupportedException>(() => McapReader.OpenMessages(new MemoryStream(bytes), new() { AllowBufferedSort = false }, options: McapReaderOptions.Strict));
        using var empty = McapReader.OpenMessages(new MemoryStream(bytes), new() { Topics = [] });
        Assert.Empty(empty.ReadMessages());
        using var interval = McapReader.OpenMessages(new MemoryStream(bytes), new() { StartTime = 1, EndTime = 2 });
        Assert.Equal(new uint[] { 1, 3 }, interval.ReadMessages().Select(m => m.Sequence));
    }
    [Fact]
    public void DuplicateChunkIndexOffsetsAreNotAcceptedAsCompleteIndexes()
    {
        byte[] data = Recording(true);
        int offset = 8; ulong? first = null;
        while (offset < data.Length - 8)
        {
            int length = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset + 1)));
            if (data[offset] == 8)
            {
                if (first is null) first = BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset + 25));
                else { BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(offset + 25), first.Value); break; }
            }
            offset += 9 + length;
        }
        // Summary CRC is optional for default readers; any parser/CRC rejection is also valid.
        Assert.Throws<McapException>(() => McapReader.OpenMessages(new MemoryStream(data), new() { AllowBufferedSort = false }));
    }
    sealed class NonSeekable(byte[] bytes) : MemoryStream(bytes)
    { public override bool CanSeek => false; }
}
