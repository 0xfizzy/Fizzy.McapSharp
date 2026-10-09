using System.Buffers.Binary;
using System.Runtime.InteropServices;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class MemoryTests
{

    static byte[] Recording(McapCompression compression, int count = 8, int size = 1024, bool chunks = true)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, UseChunks = chunks, ChunkSize = 4096 }, true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            byte[] payload = new byte[size]; Array.Fill(payload, (byte)42);
            for (int i = 0; i < count; ++i) writer.WriteMessage(new(channel, (uint)i, (ulong)(i % 3), 0), payload);
            writer.Complete();
        }
        return stream.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void DirectReadsMatchBufferAndStream(McapCompression compression)
    {
        var data = Recording(compression);
        using var buffer = new McapReadCursor(data, McapCursorMode.Messages, false);
        using var stream = new MemoryStream(data);
        using var session = McapReaderFactory.OpenMessages(stream, options: new());
        byte[] output = new byte[1024];
        for (int i = 0; i < 8; i++)
        {
            Assert.Equal(McapReadStatus.Success, buffer.ReadNext(output, out var a, out _));
            Assert.Equal(McapReadStatus.Success, session.ReadNext(output, out var b, out _));
            Assert.Equal(a, b); Assert.All(output, b => Assert.Equal((byte)42, b));
        }



        Assert.Equal(McapReadStatus.EndOfStream, buffer.ReadNext([], out _, out _));
        Assert.Equal(McapReadStatus.EndOfStream, buffer.ReadNext([], out var end, out var length));
        Assert.Equal(default, end); Assert.Equal(0ul, length);

    }

    [Fact]
    public void PendingRetriesPreserveHeaderAndDestination()
    {
        var data = Recording(McapCompression.None, 3, 1024);
        using var reader = new McapReadCursor(data, McapCursorMode.Messages, false);
        byte[] small = Enumerable.Repeat((byte)19, 20).ToArray(), output = new byte[1024];
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext(small, out var h, out var n));
        Assert.All(small, x => Assert.Equal((byte)19, x));
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext(small, out var h2, out var n2));
        Assert.Equal(h, h2); Assert.Equal(n, n2);
        Assert.Equal(McapReadStatus.Success, reader.ReadNext(output, out _, out _));
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));

        using var trimmed = new McapReadCursor(data, McapCursorMode.Messages, false);
        Assert.Equal(McapReadStatus.BufferTooSmall, trimmed.ReadNext([], out _, out _));

        trimmed.ReadNext(output, out _, out _);

        trimmed.ReadNext(output, out _, out _);

    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void MappedSnapshotSharesSourceWithIndependentCursors(McapCompression compression)
    {
        string path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        var data = Recording(compression); File.WriteAllBytes(path, data);
        try
        {
            var snapshot = McapIndexSnapshot.OpenMapped(path, new());
            var summary = snapshot.GetSummary()!;
            var index = summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0);
            using var cursor = snapshot.OpenChunkReader(index);
            using var other = snapshot.OpenChunkReader(index);

            snapshot.Dispose();
            if (OperatingSystem.IsWindows()) Assert.Throws<IOException>(() => File.Open(path, FileMode.Open, FileAccess.Write).Dispose());
            Assert.Equal(McapReadStatus.Success, cursor.ReadNext(new byte[1024], out var a, out _));
            Assert.Equal(McapReadStatus.Success, other.ReadNext(new byte[1024], out var b, out _));
            Assert.Equal(a, b);

        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void RawRecordPreservesTrailingBytesAndCanSwitchRetryInterfaces()
    {
        // A valid DataEnd body followed by extension bytes accepted by official parse_record.
        byte[] data = new byte[9 + 7]; data[0] = 15; BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(1), 7);
        data[^3] = 21; data[^2] = 22; data[^1] = 23;
        using var reader = new McapReadCursor(data, McapCursorMode.ExpandedRecordsWithoutMagic);
        byte[] output = new byte[7];
        Assert.Equal(McapReadStatus.Success, reader.ReadNextRecord(output, out var opcode, out var n));
        Assert.Equal((byte)15, opcode); Assert.Equal(7ul, n); Assert.Equal(data[9..], output);
        using var messages = new McapReadCursor(Recording(McapCompression.None));
        Assert.Equal(McapReadStatus.BufferTooSmall, messages.ReadNext([], out var h, out _));
        byte[] body = new byte[1046];
        Assert.Equal(McapReadStatus.Success, messages.ReadNextRecord(body, out opcode, out n));
        Assert.Equal(h, ((McapMessageRecord)McapRecords.Parse(opcode, body)).Header);
        Assert.Equal(McapReadStatus.BufferTooSmall, messages.ReadNextRecord([], out _, out _));
        Assert.Equal(McapReadStatus.Success, messages.ReadNext(new byte[1024], out var second, out _));
        Assert.Equal(1u, second.Sequence);
    }

    [Fact]
    public void SummaryCursorEnumeratesRecords()
    {
        using var snapshot = new McapIndexSnapshot(Recording(McapCompression.None), new());
        using var cursor = snapshot.OpenSummaryRecords();

        byte[] buffer = new byte[4096]; int count = 0;
        while (cursor.ReadNextRecord(buffer, out _, out _) != McapReadStatus.EndOfStream)
        { count++;  }
        Assert.True(count > 1);
    }

    [Theory]
    [InlineData(0)] [InlineData(32)] [InlineData(1200000)]
    public void SortAllowanceIncludesDescriptorsAndPayload(int size)
    {
        var data = Recording(McapCompression.None, 6, size, false);
        using var source = new MemoryStream(data);
        var error = Assert.Throws<McapException>(() => McapReaderFactory.OpenMessages(source,
            new() { MaxBufferedSortBytes = 0  }, true));
        Assert.Equal("BufferedSort", error.Details.GetProperty("resource").GetString());
        foreach (var order in new[] { McapReadOrder.LogTime, McapReadOrder.ReverseLogTime })
        {
            using var input = new MemoryStream(data);
            using var reader = McapReaderFactory.OpenMessages(input, new() { Order = order, MaxBufferedSortBytes = 16 * 1024 * 1024  });

            uint[] expected = [0, 3, 1, 4, 2, 5]; if (order == McapReadOrder.ReverseLogTime) Array.Reverse(expected);
            byte[] buffer = new byte[size];
            foreach (uint sequence in expected)
            {
                if (size > 0) Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));
                Assert.Equal(McapReadStatus.Success, reader.ReadNext(buffer, out var h, out _)); Assert.Equal(sequence, h.Sequence);
            }

            Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext(buffer, out _, out _));
        }
    }

    [Fact]
    public async Task AsyncDeliveryDoesNotNeedPendingAllocationWithAdequateBuffer()
    {
        using var reader = new McapAsyncReader(new MemoryStream(Recording(McapCompression.Zstd)), new());
        byte[] buffer = new byte[65536];
        while ((await reader.ReadNextRecordAsync(buffer)).Status != McapReadStatus.EndOfStream) { }

    }

    [Fact]
    public void IndexedStreamReadsAllMessages()
    {
        using var stream = new MemoryStream(Recording(McapCompression.Zstd));
        using var reader = McapReaderFactory.OpenMessages(stream, new(), options: new());
        byte[] buffer = new byte[1024]; int count = 0;
        while (reader.ReadNext(buffer, out _, out _) != McapReadStatus.EndOfStream) count++;
        Assert.Equal(8, count);


    }

    [Fact]
    public void CopiedSnapshotRetainsIndependentInputAndSessionPolicyWins()
    {
        var bytes = Recording(McapCompression.None, chunks: false);
        using var snapshot = new McapIndexSnapshot(bytes);
        Array.Clear(bytes);
        Assert.NotNull(snapshot.GetSummary());
        Assert.True(snapshot.ReadFooter().SummaryStart > 0);
        using var stream = new MemoryStream(Recording(McapCompression.None, chunks: false));
        using var reader = McapReaderFactory.OpenMessages(stream,
            new() { MaxBufferedSortBytes = 2 * 1024 * 1024 });
        Assert.Equal(8, reader.ReadMessages().Count());
    }

}
