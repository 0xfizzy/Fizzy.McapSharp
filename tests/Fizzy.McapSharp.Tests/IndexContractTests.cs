using System.Buffers.Binary;
using System.Collections.ObjectModel;
using System.Diagnostics;
using Xunit;
using Xunit.Abstractions;

namespace Fizzy.McapSharp.Tests;

public class IndexContractTests(ITestOutputHelper output)
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void OwnedMessageIndexesPreserveEmptyGroups(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression, 2);
        McapChunkIndex chunk;
        using (var original = new McapIndexSnapshot(bytes)) chunk = original.GetSummary()!.ChunkIndexes.Single();
        var emptyId = chunk.MessageIndexOffsets.Keys.Min();
        var offset = checked((int)chunk.MessageIndexOffsets[emptyId]);
        // Shorten only this random-access index record to a valid empty body. The
        // surrounding gap is intentionally not traversed; summary offsets stay fixed.
        BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(offset + 1), 6);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(offset + 11), 0);
        using var snapshot = new McapIndexSnapshot(bytes);
        using var prepared = new McapPreparedChunkIndex(chunk);
        using var session = McapReaderFactory.OpenMessages(new MemoryStream(bytes));
        var expected = session.ReadMessageIndexes(chunk).OrderBy(g => g.ChannelId).ToArray();
        foreach (var actual in new[] { snapshot.ReadMessageIndexes(chunk), snapshot.ReadMessageIndexes(prepared) })
        {
            Assert.Equal(expected.Select(g => g.ChannelId), actual.Select(g => g.ChannelId));
            Assert.Empty(actual.Single(g => g.ChannelId == emptyId).Records);
            Assert.Equal(expected.SelectMany(g => g.Records), actual.SelectMany(g => g.Records));
        }
        var onlyEmpty = chunk with { MessageIndexOffsets = new Dictionary<ushort, ulong> { [emptyId] = chunk.MessageIndexOffsets[emptyId] } };
        using var preparedEmpty = new McapPreparedChunkIndex(onlyEmpty);
        Assert.Empty(Assert.Single(snapshot.ReadMessageIndexes(onlyEmpty)).Records);
        Assert.Empty(Assert.Single(snapshot.ReadMessageIndexes(preparedEmpty)).Records);
        // The caller-buffer representation remains 18 bytes per entry, without
        // inventing a sentinel entry for the empty group.
        snapshot.ReadMessageIndexes(chunk, [], out var size);
        Assert.Equal((ulong)expected.Sum(g => g.Records.Count) * 18, size);
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void CacheAndBatchIdentityIgnoreMapInsertionOrder(McapCompression compression)
    {
        var bytes = DeliveryOptimizationTests.Recording(compression, 2);
        using var snapshot = new McapIndexSnapshot(bytes, new() { MaxRandomAccessCacheBytes = 1024 * 1024 });
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        var reordered = chunk with { MessageIndexOffsets = chunk.MessageIndexOffsets.Reverse().ToDictionary(e => e.Key, e => e.Value) };
        var entry = snapshot.ReadMessageIndexes(chunk)[0].Records[0];
        snapshot.SeekMessage(chunk, entry);
        snapshot.SeekMessage(reordered, entry);
        Assert.Equal(1UL, snapshot.GetCacheStatistics().ChunkLoads);
        using var a = new McapPreparedChunkIndex(chunk);
        using var b = new McapPreparedChunkIndex(reordered);
        using var uncached = new McapIndexSnapshot(bytes);
        using var batch = uncached.SeekMessages([new(a, entry), new(b, entry)]);
        Assert.Equal(1UL, uncached.GetCacheStatistics().ChunkLoads);
        Assert.Equal(batch.GetPayload(0).ToArray(), batch.GetPayload(1).ToArray());
        // Canonicalization must not reduce identity to only the file offset.
        Assert.Equal(McapErrorKind.BadIndex, Assert.Throws<McapException>(() =>
            snapshot.SeekMessage(reordered with { ChunkLength = ulong.MaxValue }, entry)).Kind);
    }

    [Fact]
    public void SortedListEncodingObservesCurrentEntries()
    {
        var bytes = DeliveryOptimizationTests.Recording(McapCompression.None, 2);
        using var snapshot = new McapIndexSnapshot(bytes);
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        var entries = chunk.MessageIndexOffsets.ToArray();
        var offsets = new SortedList<ushort, ulong> { [entries[0].Key] = entries[0].Value };
        var selected = chunk with { MessageIndexOffsets = offsets };
        Assert.Equal(entries[0].Key, Assert.Single(snapshot.ReadMessageIndexes(selected)).ChannelId);
        offsets.Clear();
        offsets.Add(entries[1].Key, entries[1].Value);
        Assert.Equal(entries[1].Key, Assert.Single(snapshot.ReadMessageIndexes(selected)).ChannelId);
    }

    [Fact]
    public void SparseHighIdMapProfile()
    {
        var bytes = DeliveryOptimizationTests.Recording(McapCompression.None);
        using var snapshot = new McapIndexSnapshot(bytes);
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        var offsets = new Dictionary<ushort, ulong> { [ushort.MaxValue] = 123 };
        var direct = chunk with { MessageIndexOffsets = offsets };
        var readOnly = chunk with { MessageIndexOffsets = new ReadOnlyDictionary<ushort, ulong>(offsets) };
        var sorted = chunk with { MessageIndexOffsets = new SortedDictionary<ushort, ulong>(offsets) };
        var sortedList = chunk with { MessageIndexOffsets = new SortedList<ushort, ulong>(offsets) };
        var sortedListBaseline = chunk with { MessageIndexOffsets = new ReadOnlyDictionary<ushort, ulong>(new SortedList<ushort, ulong>(offsets)) };
        using var prepared = new McapPreparedChunkIndex(readOnly);
        // Offset calculation uses the descriptor without reading its message-index
        // records, isolating repeated descriptor work from payload/decompression.
        Measure("Dictionary", () => snapshot.GetCompressedDataOffset(direct));
        Measure("ReadOnlyDictionary", () => snapshot.GetCompressedDataOffset(readOnly), false);
        Measure("SortedDictionary", () => snapshot.GetCompressedDataOffset(sorted), false, 20);
        var expectedOffset = snapshot.GetCompressedDataOffset(chunk);
        Measure("SortedDictionary lookup-only baseline", () =>
        {
            for (int id = 0; id <= ushort.MaxValue; id++) sorted.MessageIndexOffsets.TryGetValue((ushort)id, out _);
            return expectedOffset;
        }, false, 20);
        Measure("SortedList indexed access", () => snapshot.GetCompressedDataOffset(sortedList));
        Measure("SortedList interface-scan baseline", () => snapshot.GetCompressedDataOffset(sortedListBaseline), false, 20);
        Measure("SortedList lookup-only baseline", () =>
        {
            for (int id = 0; id <= ushort.MaxValue; id++) sortedListBaseline.MessageIndexOffsets.TryGetValue((ushort)id, out _);
            return expectedOffset;
        }, false, 20);
        Measure("Prepared ReadOnlyDictionary", () => snapshot.GetCompressedDataOffset(prepared));
        void Measure(string name, Func<ulong> action, bool requireZeroAllocation = true, int iterations = 1000)
        {
            // Interface lookups and comparers are supplied by the BCL/caller;
            // their allocations may change with runtime tiering. Report those
            // observations without extending the binding's allocation gates.
            for (int i = 0; i < Math.Min(100, iterations); i++) action();
            long before = GC.GetAllocatedBytesForCurrentThread();
            long start = Stopwatch.GetTimestamp();
            ulong value = 0;
            for (int i = 0; i < iterations; i++) value = action();
            var elapsed = Stopwatch.GetElapsedTime(start);
            long allocation = GC.GetAllocatedBytesForCurrentThread() - before;
            Assert.Equal(snapshot.GetCompressedDataOffset(chunk), value);
            if (requireZeroAllocation) Assert.Equal(0, allocation);
            output.WriteLine($"{name}: {elapsed.TotalMilliseconds:F3} ms/{iterations} calls; {elapsed.TotalMilliseconds * 1000 / iterations:F3} us/call; managed bytes={allocation}; bytes/call={(double)allocation / iterations:F1}");
        }
    }
}
