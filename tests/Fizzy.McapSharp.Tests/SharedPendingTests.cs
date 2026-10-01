using Xunit;

namespace Fizzy.McapSharp.Tests;

public class SharedPendingTests
{
    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void BufferRetryPreservesBodyAndCopiesOnlyTheChosenDelivery(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression);
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var reader = new McapBufferReader(bytes, McapBufferReadMode.Messages, false, options: new() { Budget = budget, MaxPendingBufferBytes = 70022 });
        var before = reader.GetMemoryStatistics();
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var header, out var length));
        Assert.Equal(70000UL, length);
        Assert.Equal(before.CopiedBytes, reader.GetMemoryStatistics().CopiedBytes);
        var pending = budget.GetDetailedStatistics();
        var sentinel = Enumerable.Repeat((byte)77, 31).ToArray();
        for (int i = 0; i < 3; i++)
        {
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord(sentinel, out var opcode, out length));
            Assert.Equal(5, opcode); Assert.Equal(70022UL, length);
            Assert.All(sentinel, b => Assert.Equal(77, b));
            Assert.Equal(pending, budget.GetDetailedStatistics());
        }
        byte[] body = new byte[70022];
        Assert.Equal(McapReadStatus.Message, reader.ReadNextRecord(body, out var op, out _));
        Assert.Equal(header, ((McapMessageRecord)McapRecords.Parse(op, body)).Header);
        Assert.Equal(pending.Flow.DeliveryCopyBytes + 70022, budget.GetDetailedStatistics().Flow.DeliveryCopyBytes);
        Assert.Equal(pending.Flow.OtherCopyBytes, budget.GetDetailedStatistics().Flow.OtherCopyBytes);
        // Empty message body remains exchangeable with its empty payload.
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord([], out _, out _));
        Assert.Equal(McapReadStatus.Message, reader.ReadNext([], out _, out length));
        Assert.Equal(0UL, length);
        reader.Dispose();
        BudgetAssertions.Idle(budget);
    }

    [Theory]
    [InlineData(McapCompression.None, false)] [InlineData(McapCompression.Lz4, false)] [InlineData(McapCompression.Zstd, false)]
    [InlineData(McapCompression.None, true)] [InlineData(McapCompression.Lz4, true)] [InlineData(McapCompression.Zstd, true)]
    public void PendingCanTransferToLeaseWithoutPayloadCopy(McapCompression compression, bool indexed)
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        var bytes = DeliveryOptimizationTests.Recording(compression);
        using var reader = McapReader.OpenMessages(new MemoryStream(bytes), indexed ? new() : null,
            options: new() { Memory = new() { Budget = budget, MaxPendingBufferBytes = 70000 } });
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var expected, out _));
        var before = budget.GetDetailedStatistics();
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out _, out _));
        Assert.Equal(before, budget.GetDetailedStatistics());
        using var lease = reader.ReadBatchLease(1)!;
        Assert.Equal(expected, lease.GetHeader(0));
        Assert.Equal(70000, lease.GetPayload(0).Length);
        Assert.Equal(before.Flow.DeliveryCopyBytes, budget.GetDetailedStatistics().Flow.DeliveryCopyBytes);
        Assert.Equal(before.Flow.OtherCopyBytes, budget.GetDetailedStatistics().Flow.OtherCopyBytes);
        reader.Dispose();
        Assert.True(budget.GetStatistics().CurrentBytes > 0);
        Assert.Equal(0, lease.GetPayload(0)[69999]);
        lease.Dispose();
        BudgetAssertions.Idle(budget);
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void BufferPendingCanBecomeBorrowedOrOwnedOrLease(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression);
        foreach (int mode in new[] { 0, 1, 2 })
        {
            var budget = new McapMemoryBudget(maxRetainedBytes: 0);
            using var reader = new McapBufferReader(bytes, McapBufferReadMode.Messages, false, options: new() { Budget = budget });
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextRecord([], out _, out _));
            var before = budget.GetDetailedStatistics().Flow;
            if (mode == 0)
            {
                int length = 0;
                Assert.Equal(McapReadStatus.Message, reader.ReadNext((in McapMessageHeader h, ReadOnlySpan<byte> data) => { length = data.Length; return true; }));
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
            var after = budget.GetDetailedStatistics().Flow;
            Assert.Equal(before.OtherCopyBytes, after.OtherCopyBytes);
            // Owned enumeration delivers the payload and the fixture's one-byte schema.
            Assert.Equal(before.DeliveryCopyBytes + (mode == 1 ? 70001UL : 0UL), after.DeliveryCopyBytes);
            reader.Dispose(); BudgetAssertions.Idle(budget);
        }
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public async Task AsyncRecordRetriesRetainStorageAndCountOnlyDelivery(McapCompression compression)
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var reader = new McapAsyncReader(new MemoryStream(DeliveryOptimizationTests.Recording(compression)),
            new() { Memory = new() { Budget = budget } });
        while (true)
        {
            var first = await reader.ReadNextRecordAsync(Memory<byte>.Empty);
            if (first.Status == McapReadStatus.EndOfStream) break;
            var pending = budget.GetDetailedStatistics();
            var retry = await reader.ReadNextRecordAsync(Memory<byte>.Empty);
            Assert.Equal(first, retry); Assert.Equal(pending, budget.GetDetailedStatistics());
            var output = new byte[checked((int)first.Length)];
            var final = await reader.ReadNextRecordAsync(output);
            Assert.Equal(McapReadStatus.Message, final.Status);
            Assert.Equal(pending.Flow.DeliveryCopyBytes + (ulong)output.Length, budget.GetDetailedStatistics().Flow.DeliveryCopyBytes);
            Assert.Equal(pending.Flow.OtherCopyBytes, budget.GetDetailedStatistics().Flow.OtherCopyBytes);
        }
        reader.Dispose(); BudgetAssertions.Idle(budget);
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
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var reader = McapReader.OpenMessages(new ShortReadStream(bytes), new(),
            options: new() { Memory = new() { Budget = budget, MaxScratchBufferBytes = chunks.Max(c => c.CompressedSize) } });
        var before = budget.GetDetailedStatistics().Flow;
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
            var flow = budget.GetDetailedStatistics().Flow;
            Assert.Equal(before.DeliveryCopyBytes, flow.DeliveryCopyBytes);
            Assert.Equal(before.OtherCopyBytes, flow.OtherCopyBytes);
            Assert.Equal(before.InputCopyBytes + chunks.Aggregate(0UL, (total, chunk) => total + chunk.CompressedSize), flow.InputCopyBytes);
            reader.Dispose();
            Assert.Equal(18, leases.Select(l => l.GetHeader(0).Sequence).Distinct().Count());
            foreach (var lease in leases)
                Assert.True(lease.GetPayload(0).ToArray().All(b => b == lease.GetHeader(0).Sequence + 1));
        }
        finally { foreach (var lease in leases) lease.Dispose(); }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void IndexedStableInputHonorsScratchLimitAndTruncatedReadFails()
    {
        var bytes = OverlappingRecording();
        using var snapshot = new McapIndexSnapshot(bytes);
        var chunks = snapshot.GetSummary()!.ChunkIndexes;
        using (var reader = McapReader.OpenMessages(new ShortReadStream(bytes), new(),
            options: new() { Memory = new() { MaxScratchBufferBytes = chunks.Max(c => c.CompressedSize) - 1 } }))
        {
            var error = Assert.Throws<McapException>(() => { while (reader.ReadNext(new byte[256], out _, out _) != McapReadStatus.EndOfStream) { } });
            Assert.Equal("ScratchBuffer", error.Details.GetProperty("resource").GetString());
        }
        var first = chunks[0];
        long start = checked((long)snapshot.GetCompressedDataOffset(first));
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var truncated = McapReader.OpenMessages(new ShortReadStream(bytes, start + 19, start + (long)first.CompressedSize), new(),
            options: new() { Memory = new() { Budget = budget } });
        Assert.Throws<McapException>(() => truncated.ReadNext(new byte[256], out _, out _));
        Assert.Throws<InvalidOperationException>(() => truncated.ReadNext(new byte[256], out _, out _));
        truncated.Dispose(); BudgetAssertions.Idle(budget);
    }

}
