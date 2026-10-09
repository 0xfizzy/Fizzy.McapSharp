using Xunit;

namespace Fizzy.McapSharp.Tests;

public class SharedPendingTests
{
    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void BufferRetryPreservesBodyAndCopiesOnlyTheChosenDelivery(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression);
        using var reader = new McapBufferReader(bytes, McapBufferReadMode.Messages, false);
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var header, out var length));
        Assert.Equal(70000UL, length);

        var sentinel = Enumerable.Repeat((byte)77, 31).ToArray();
        for (int i = 0; i < 3; i++)
        {
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord(sentinel, out var opcode, out length));
            Assert.Equal(5, opcode); Assert.Equal(70022UL, length);
            Assert.All(sentinel, b => Assert.Equal(77, b));

        }
        byte[] body = new byte[70022];
        Assert.Equal(McapReadStatus.Success, reader.ReadNextRecord(body, out var op, out _));
        Assert.Equal(header, ((McapMessageRecord)McapRecords.Parse(op, body)).Header);

        // Empty message body remains exchangeable with its empty payload.
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord([], out _, out _));
        Assert.Equal(McapReadStatus.Success, reader.ReadNext([], out _, out length));
        Assert.Equal(0UL, length);
        reader.Dispose();
    }

    [Theory]
    [InlineData(McapCompression.None, false)] [InlineData(McapCompression.Lz4, false)] [InlineData(McapCompression.Zstd, false)]
    [InlineData(McapCompression.None, true)] [InlineData(McapCompression.Lz4, true)] [InlineData(McapCompression.Zstd, true)]
    public void PendingCanTransferToLeaseWithoutPayloadCopy(McapCompression compression, bool indexed)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression);
        using var reader = McapFileReader.OpenMessages(new MemoryStream(bytes), indexed ? new() : null,
            options: new());
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var expected, out _));
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));

        using var lease = reader.ReadBatchLease(1)!;
        Assert.Equal(expected, lease.GetHeader(0));
        Assert.Equal(70000, lease.GetPayload(0).Length);

        reader.Dispose();

        Assert.Equal(0, lease.GetPayload(0)[69999]);
        lease.Dispose();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void BufferPendingCanBecomeBorrowedOrOwnedOrLease(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression);
        foreach (int mode in new[] { 0, 1, 2 })
        {
            using var reader = new McapBufferReader(bytes, McapBufferReadMode.Messages, false);
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord([], out _, out _));
            if (mode == 0)
            {
                int length = 0;
                Assert.Equal(McapReadStatus.Success, reader.ReadNext((in McapMessageHeader h, ReadOnlySpan<byte> data) => { length = data.Length; return true; }));
                Assert.Equal(70000, length);
            }
            else if (mode == 1)
            {
                using var enumerator = reader.ReadMessages().GetEnumerator();
                Assert.True(enumerator.MoveNext()); Assert.Equal(70000, enumerator.Current.Data.Length);
            }
            else
            {
                using var lease = reader.ReadBatchLease(1)!;
                reader.Dispose(); Assert.Equal(70000, lease.GetPayload(0).Length);
            }

        }
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public async Task AsyncRecordRetriesPreserveResults(McapCompression compression)
    {
        using var reader = new McapAsyncReader(new MemoryStream(DeliveryOptimizationTests.Recording(compression)),
            new());
        while (true)
        {
            var first = await reader.ReadNextRecordAsync(Memory<byte>.Empty);
            if (first.Status == McapReadStatus.EndOfStream) break;
            var retry = await reader.ReadNextRecordAsync(Memory<byte>.Empty);
            Assert.Equal(first, retry);
            var output = new byte[checked((int)first.Length)];
            var final = await reader.ReadNextRecordAsync(output);
            Assert.Equal(McapReadStatus.Success, final.Status);

        }
    }
    static byte[] OverlappingRecording()
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = McapCompression.None, ChunkSize = 900 }, true))
        {
            var channel = writer.RegisterChannel("overlap", "raw");
            for (uint i = 0; i < 18; i++)
                writer.WriteMessage(new(channel, i, (i % 3) * 100 + i / 3, 0), Enumerable.Repeat((byte)(i + 1), 256).ToArray());
            writer.Complete();
        }
        return output.ToArray();
    }

    sealed class ShortReadStream(byte[] bytes, long stopStart = -1, long stopEnd = -1) : MemoryStream(bytes)
    {
        public override int Read(Span<byte> buffer)
        {
            if (Position >= stopStart && Position < stopEnd) return 0;
            return base.Read(buffer[..Math.Min(buffer.Length, 19)]);
        }
    }

    [Fact]
    public void IndexedShortReadsRetainDistinctOverlappingChunksWithoutCopies()
    {
        var bytes = OverlappingRecording();
        using var snapshot = new McapIndexSnapshot(bytes);
        var chunks = snapshot.GetSummary()!.ChunkIndexes;
        Assert.True(chunks.Count > 2);
        using var reader = McapFileReader.OpenMessages(new ShortReadStream(bytes), new(),
            options: new());
        var leases = new List<McapMessageBatchLease>();
        try
        {
            ulong previous = 0;
            while (reader.ReadBatchLease(1) is { } batch)
            {
                leases.Add(batch);
                Assert.True(batch.GetHeader(0).LogTime >= previous);
                previous = batch.GetHeader(0).LogTime;
            }
            Assert.Equal(18, leases.Count);

            reader.Dispose();
            Assert.Equal(18, leases.Select(l => l.GetHeader(0).Sequence).Distinct().Count());
            foreach (var lease in leases)
                Assert.True(lease.GetPayload(0).ToArray().All(b => b == lease.GetHeader(0).Sequence + 1));
        }
        finally { foreach (var lease in leases) lease.Dispose(); }
    }

    [Fact]
    public void TruncatedIndexedStreamReadTerminatesSession()
    {
        var bytes = OverlappingRecording();
        using var snapshot = new McapIndexSnapshot(bytes);
        var first = snapshot.GetSummary()!.ChunkIndexes[0];
        long start = checked((long)snapshot.GetCompressedDataOffset(first));
        using var truncated = McapFileReader.OpenMessages(new ShortReadStream(bytes, start + 19, start + (long)first.CompressedSize), new());
        Assert.Throws<McapException>(() => truncated.ReadNext(new byte[256], out _, out _));
        Assert.Throws<InvalidOperationException>(() => truncated.ReadNext(new byte[256], out _, out _));
    }

}
