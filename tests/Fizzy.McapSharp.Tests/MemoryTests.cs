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
    public void DirectReadsNeedNoPendingCapacity(McapCompression compression)
    {
        var data = Recording(compression);
        var memory = new McapMemoryOptions { MaxPendingBufferBytes = 0 };
        using var buffer = new McapBufferReader(data, McapBufferReadMode.Messages, false, memory);
        using var stream = new MemoryStream(data);
        using var session = McapReader.OpenMessages(stream, options: new() { Memory = memory });
        byte[] output = new byte[1024];
        for (int i = 0; i < 8; i++)
        {
            Assert.Equal(McapReadStatus.Message, buffer.ReadNext(output, out var a, out _));
            Assert.Equal(McapReadStatus.Message, session.ReadNext(output, out var b, out _));
            Assert.Equal(a, b); Assert.All(output, b => Assert.Equal((byte)42, b));
        }
        Assert.Equal((ulong)data.Length, buffer.GetMemoryStatistics().CurrentControlledBytes);
        Assert.Equal(0ul, session.GetMemoryStatistics().CurrentControlledBytes);
        Assert.Equal(0ul, session.GetMemoryStatistics().AllocationCount);
        Assert.Equal(McapReadStatus.EndOfStream, buffer.ReadNext([], out _, out _));
        var before = buffer.GetMemoryStatistics();
        Assert.Equal(McapReadStatus.EndOfStream, buffer.ReadNext([], out var end, out var length));
        Assert.Equal(default, end); Assert.Equal(0ul, length);
        Assert.Equal(before, buffer.GetMemoryStatistics());
    }

    [Fact]
    public void PendingRetriesReuseCapacityAndOversizeBuffersAreReleased()
    {
        var data = Recording(McapCompression.None, 3, 1024);
        using var reader = new McapBufferReader(data, McapBufferReadMode.Messages, false, new() { MaxPendingBufferBytes = 1046, MaxRetainedBufferBytes = 1046 });
        byte[] small = Enumerable.Repeat((byte)19, 20).ToArray(), output = new byte[1024];
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext(small, out var h, out var n));
        var held = reader.GetMemoryStatistics();
        Assert.All(small, x => Assert.Equal((byte)19, x));
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext(small, out var h2, out var n2));
        Assert.Equal(h, h2); Assert.Equal(n, n2); Assert.Equal(held, reader.GetMemoryStatistics());
        Assert.Equal(McapReadStatus.Message, reader.ReadNext(output, out _, out _));
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));
        Assert.Equal(held.AllocationCount, reader.GetMemoryStatistics().AllocationCount);
        using var trimmed = new McapBufferReader(data, McapBufferReadMode.Messages, false, new() { MaxRetainedBufferBytes = 32 });
        Assert.Equal(McapReadStatus.BufferTooSmall, trimmed.ReadNext([], out _, out _));
        Assert.True(trimmed.GetMemoryStatistics().CurrentControlledBytes > (ulong)data.Length);
        trimmed.ReadNext(output, out _, out _);
        Assert.Equal((ulong)data.Length, trimmed.GetMemoryStatistics().CurrentControlledBytes);
        trimmed.ReadNext(output, out _, out _);
        Assert.Equal((ulong)data.Length, trimmed.GetMemoryStatistics().CurrentControlledBytes);
    }

    [Fact]
    public void BudgetErrorsAreStructuredAndTerminalButDescriptionsSurvive()
    {
        var data = Recording(McapCompression.None);
        using var reader = new McapBufferReader(data, McapBufferReadMode.Messages, false, new() { MaxPendingBufferBytes = 0 });
        var ex = Assert.Throws<McapException>(() => reader.ReadNext([], out _, out _));
        Assert.Equal(McapErrorKind.Binding, ex.Kind);
        Assert.Equal("PendingBuffer", ex.Details.GetProperty("resource").GetString());
        Assert.Equal(0ul, ex.Details.GetProperty("limit").GetUInt64());
        Assert.Equal(1046ul, ex.Details.GetProperty("requested").GetUInt64());
        Assert.Equal("topic", reader.GetChannel(1).Topic);
        Assert.Throws<McapException>(() => reader.ReadNext(new byte[1024], out _, out _));
        Assert.Equal((ulong)data.Length, reader.GetMemoryStatistics().CurrentControlledBytes);
        Assert.Throws<McapException>(() => new McapBufferReader(data, McapBufferReadMode.Messages, false, new() { MaxOwnedInputBytes = (ulong)data.Length - 1 }));
        using var exact = new McapIndexSnapshot(data, new() { MaxOwnedInputBytes = (ulong)data.Length });
        Assert.Equal((ulong)data.Length, exact.GetMemoryStatistics().CurrentControlledBytes);
    }

    [Fact]
    public void FailedSnapshotBudgetRestoresSourceAndPendingMessage()
    {
        var data = Recording(McapCompression.None);
        using var stream = new MemoryStream(data);
        using var reader = McapReader.OpenMessages(stream, leaveOpen: true);
        reader.ReadNext([], out var first, out _);
        long position = stream.Position;
        Assert.Throws<McapException>(() => reader.OpenIndexSnapshot(new() { MaxOwnedInputBytes = 0 }));
        Assert.Equal(position, stream.Position);
        Assert.Equal(McapReadStatus.Message, reader.ReadNext(new byte[1024], out var retry, out _));
        Assert.Equal(first, retry);
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void MappedSnapshotSharesSourceWithIndependentCursors(McapCompression compression)
    {
        string path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        var data = Recording(compression); File.WriteAllBytes(path, data);
        try
        {
            var snapshot = McapIndexSnapshot.OpenMapped(path, new() { MaxOwnedInputBytes = 0, MaxPendingBufferBytes = 0 });
            var summary = snapshot.GetSummary()!;
            var index = summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0);
            using var cursor = snapshot.OpenChunkReader(index);
            using var other = snapshot.OpenChunkReader(index);
            var statistics = snapshot.GetMemoryStatistics();
            Assert.Equal(0ul, statistics.CurrentControlledBytes); Assert.Equal((ulong)data.Length, statistics.MappedBytes);
            snapshot.Dispose();
            if (OperatingSystem.IsWindows()) Assert.Throws<IOException>(() => File.Open(path, FileMode.Open, FileAccess.Write).Dispose());
            Assert.Equal(McapReadStatus.Message, cursor.ReadNext(new byte[1024], out var a, out _));
            Assert.Equal(McapReadStatus.Message, other.ReadNext(new byte[1024], out var b, out _));
            Assert.Equal(a, b); Assert.Equal((ulong)data.Length, cursor.GetMemoryStatistics().MappedBytes);
            Assert.Equal(0ul, cursor.GetMemoryStatistics().CurrentControlledBytes);
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void RawRecordPreservesTrailingBytesAndCanSwitchRetryInterfaces()
    {
        // A valid DataEnd body followed by extension bytes accepted by official parse_record.
        byte[] data = new byte[9 + 7]; data[0] = 15; BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(1), 7);
        data[^3] = 21; data[^2] = 22; data[^1] = 23;
        using var reader = new McapBufferReader(data, McapBufferReadMode.SansMagic);
        byte[] output = new byte[7];
        Assert.Equal(McapReadStatus.Message, reader.ReadNextRecord(output, out var opcode, out var n));
        Assert.Equal((byte)15, opcode); Assert.Equal(7ul, n); Assert.Equal(data[9..], output);
        using var messages = new McapBufferReader(Recording(McapCompression.None));
        Assert.Equal(McapReadStatus.BufferTooSmall, messages.ReadNext([], out var h, out _));
        byte[] body = new byte[1046];
        Assert.Equal(McapReadStatus.Message, messages.ReadNextRecord(body, out opcode, out n));
        Assert.Equal(h, ((McapMessageRecord)McapRecords.Parse(opcode, body)).Header);
        Assert.Equal(McapReadStatus.BufferTooSmall, messages.ReadNextRecord([], out _, out _));
        Assert.Equal(McapReadStatus.Message, messages.ReadNext(new byte[1024], out var second, out _));
        Assert.Equal(1u, second.Sequence);
    }

    [Fact]
    public void SummaryEncodingIsLazyAndRetainedCapacityIsBounded()
    {
        using var snapshot = new McapIndexSnapshot(Recording(McapCompression.None), new() { MaxRetainedBufferBytes = 0 });
        using var cursor = snapshot.OpenSummaryRecords();
        Assert.Equal(0ul, cursor.GetMemoryStatistics().AllocationCount);
        byte[] buffer = new byte[4096]; int count = 0;
        while (cursor.ReadNextRecord(buffer, out _, out _) != McapReadStatus.EndOfStream)
        { count++; Assert.Equal(0ul, cursor.GetMemoryStatistics().CurrentControlledBytes); }
        Assert.True(count > 1);
    }

    [Theory]
    [InlineData(0)] [InlineData(32)] [InlineData(1200000)]
    public void SortBudgetAndOrderAccountForDescriptorsAndPayload(int size)
    {
        var data = Recording(McapCompression.None, 6, size, false);
        using var source = new MemoryStream(data);
        var error = Assert.Throws<McapException>(() => McapReader.OpenMessages(source,
            new() { Memory = new() { MaxBufferedSortBytes = 0 } }, true));
        Assert.Equal("BufferedSort", error.Details.GetProperty("resource").GetString());
        foreach (var order in new[] { McapReadOrder.LogTime, McapReadOrder.ReverseLogTime })
        {
            using var input = new MemoryStream(data);
            using var reader = McapReader.OpenMessages(input, new() { Order = order, Memory = new() { MaxBufferedSortBytes = 16 * 1024 * 1024 } });
            var before = reader.GetMemoryStatistics();
            Assert.True(before.CurrentControlledBytes > 0);
            uint[] expected = [0, 3, 1, 4, 2, 5]; if (order == McapReadOrder.ReverseLogTime) Array.Reverse(expected);
            byte[] buffer = new byte[size];
            foreach (uint sequence in expected)
            {
                if (size > 0) Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));
                Assert.Equal(McapReadStatus.Message, reader.ReadNext(buffer, out var h, out _)); Assert.Equal(sequence, h.Sequence);
            }
            Assert.Equal(0ul, reader.GetMemoryStatistics().CurrentControlledBytes);
            Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext(buffer, out _, out _));
        }
    }

    [Fact]
    public async Task AsyncDeliveryDoesNotNeedPendingAllocationWithAdequateBuffer()
    {
        using var reader = new McapAsyncReader(new MemoryStream(Recording(McapCompression.Zstd)), new() { Memory = new() { MaxPendingBufferBytes = 0 } });
        byte[] buffer = new byte[65536];
        while ((await reader.ReadNextRecordAsync(buffer)).Status != McapReadStatus.EndOfStream) { }
        Assert.Equal(0ul, reader.GetMemoryStatistics().AllocationCount);
        Assert.Equal(40, Marshal.SizeOf<McapMemoryStatistics>());
    }

    [Fact]
    public void IndexedStreamScratchDoesNotConsumePendingBudget()
    {
        using var stream = new MemoryStream(Recording(McapCompression.Zstd));
        using var reader = McapReader.OpenMessages(stream, new(), options: new() { Memory = new() { MaxPendingBufferBytes = 0, MaxRetainedBufferBytes = 0 } });
        byte[] buffer = new byte[1024]; int count = 0;
        while (reader.ReadNext(buffer, out _, out _) != McapReadStatus.EndOfStream) count++;
        Assert.Equal(8, count);
        Assert.Equal(0ul, reader.GetMemoryStatistics().CurrentControlledBytes);
        Assert.True(reader.GetMemoryStatistics().PeakControlledBytes > 0);
    }

    [Fact]
    public void CopiedSnapshotRetainsIndependentInputAndSessionPolicyWins()
    {
        var bytes = Recording(McapCompression.None, chunks: false);
        using var snapshot = new McapIndexSnapshot(bytes);
        Array.Clear(bytes);
        Assert.NotNull(snapshot.GetSummary());
        Assert.NotNull(snapshot.ReadFooter());
        using var stream = new MemoryStream(Recording(McapCompression.None, chunks: false));
        using var reader = McapReader.OpenMessages(stream,
            new() { Memory = new() { MaxBufferedSortBytes = 0 } },
            options: new() { Memory = new() { MaxBufferedSortBytes = 2 * 1024 * 1024 } });
        Assert.Equal(8, reader.ReadMessages().Count());
    }

    [Fact]
    public async Task AsyncPendingBudgetFailureTerminatesSession()
    {
        using var reader = new McapAsyncReader(new MemoryStream(Recording(McapCompression.None)), new() { Memory = new() { MaxPendingBufferBytes = 0 } });
        var error = await Assert.ThrowsAsync<McapException>(async () => await reader.ReadNextRecordAsync(Memory<byte>.Empty));
        Assert.Equal("PendingBuffer", error.Details.GetProperty("resource").GetString());
        Assert.Equal(0ul, reader.GetMemoryStatistics().CurrentControlledBytes);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[65536]));
    }
}
