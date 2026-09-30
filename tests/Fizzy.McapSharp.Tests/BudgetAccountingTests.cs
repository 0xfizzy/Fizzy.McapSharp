using Xunit;
namespace Fizzy.McapSharp.Tests;
public class BudgetAccountingTests
{
    [Fact]
    public void DetailedStatisticsHaveStableAbiLayout()
    {
        Assert.Equal(32,System.Runtime.InteropServices.Marshal.SizeOf<McapResourceStatistics>());
        Assert.Equal(104,System.Runtime.InteropServices.Marshal.SizeOf<McapBudgetFlowStatistics>());
        Assert.Equal(456,System.Runtime.InteropServices.Marshal.SizeOf<McapDetailedBudgetStatistics>());
    }
    [Theory]
    [InlineData(McapCompression.Lz4, 0)]
    [InlineData(McapCompression.Zstd, 0)]
    [InlineData(McapCompression.Zstd, 1)]
    [InlineData(McapCompression.Zstd, 2)]
    public void CodecAllocationsAreChargedAndReleased(McapCompression compression, uint threads)
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, CompressionThreads = threads, ChunkSize = 8 * 1024 * 1024, Memory = new() { Budget = budget } }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            writer.WriteMessage(new(channel, 1, 1, 0), new byte[1024 * 1024]);
            var detail = budget.GetDetailedStatistics();
            Assert.True(detail.CodecEncoder.LiveBytes > 0);
            CheckTotals(budget);
            writer.Complete();
        }
        Assert.Equal(0UL, budget.GetStatistics().CurrentBytes);
        stream.Position = 0;
        using (var reader = McapReader.OpenMessages(stream, new() { Order = McapReadOrder.File }, true, new() { Memory = new() { Budget = budget } }))
        {
            using var batch = reader.ReadBatchLease(1)!;
            Assert.True(budget.GetDetailedStatistics().CodecDecoder.LiveBytes > 0);
            Assert.Equal(1024 * 1024, batch.GetPayload(0).Length);
            var leased=budget.GetDetailedStatistics().ActiveLeasePayloadBytes;
            Assert.True(leased>=1024*1024);
            using(var retained=batch.RetainMessage(0)) Assert.Equal(leased,budget.GetDetailedStatistics().ActiveLeasePayloadBytes);
            var flow=budget.GetDetailedStatistics().Flow;
            Assert.True(flow.DecompressionsStarted>0);
            Assert.True(flow.DecodedOutputBytes>=1024*1024);
            CheckTotals(budget);
        }
        Assert.Equal(0UL, budget.GetStatistics().CurrentBytes);
        Assert.Equal(0UL, budget.GetDetailedStatistics().CodecDecoder.LiveBytes);
    }

    [Fact]
    public void AnotherReaderCanReclaimCachedStorage()
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid()+".mcap");
        try {
            using (var writer = new McapWriter(path, new() { Compression = McapCompression.None })) {
                var channel = writer.RegisterChannel("t", "raw");
                writer.WriteMessage(new(channel, 1, 1, 0), new byte[70000]); writer.Complete();
            }
            var budget = new McapMemoryBudget(180000, 180000, 0);
            using var reader = McapIndexSnapshot.OpenMapped(path, new() { Budget = budget, MaxRandomAccessCacheBytes = 1000000 });
            using var index = new McapPreparedChunkIndex(reader.GetSummary()!.ChunkIndexes[0]);
            var entry = reader.ReadMessageIndexes(index)[0].Records[0];
            reader.SeekMessage(index, entry, static (in McapMessageHeader h, ReadOnlySpan<byte> data) => true);
            var used = budget.GetStatistics().CurrentBytes;
            Assert.True(used > 60000);
            // Copied input competes with this reader's recyclable index/descriptor pages.
            var bytes = new byte[checked((int)(budget.MaxBytes - used + 32768))];
            Assert.True((ulong)bytes.Length <= budget.MaxBlockBytes);
            using (var input = new McapBufferReader(bytes, McapBufferReadMode.Messages, false, new() { Budget = budget })) {
                Assert.InRange(budget.GetStatistics().CurrentBytes, 0UL, budget.MaxBytes);
            }
            reader.SeekMessage(index, entry, static (in McapMessageHeader h, ReadOnlySpan<byte> data) => true);
            Assert.Equal(2UL, reader.GetCacheStatistics().ChunkLoads);
        } finally { File.Delete(path); }
    }
    [Fact]
    public void SummaryPageFailureLeavesWriterTerminalAndReleasable()
    {
        var budget = new McapMemoryBudget(32768, 32768, 0);
        using var stream = new MemoryStream();
        using(var writer = new McapWriter(stream,new(){Compression=McapCompression.None, EmitMessageIndexes=false, Memory=new(){Budget=budget}},true))
        {
            var channel=writer.RegisterChannel("t","raw");
            writer.WriteMessage(new(channel,1,1,0),new byte[16]);
            Assert.Throws<McapException>(()=>writer.Complete());
            Assert.Throws<InvalidOperationException>(()=>writer.Complete());
        }
        Assert.Equal(0UL,budget.GetStatistics().CurrentBytes);
    }

    static void CheckTotals(McapMemoryBudget budget)
    {
        var d = budget.GetDetailedStatistics();
        var categories = new[] { d.Input, d.Decompressed, d.Writer, d.CodecEncoder, d.CodecDecoder, d.Index, d.Descriptor, d.Declaration, d.Scratch };
        Assert.Equal(budget.GetStatistics().CurrentBytes, categories.Aggregate(0UL, (n, c) => n + c.CurrentBytes));
        foreach (var category in categories) Assert.Equal(category.CurrentBytes, category.LiveBytes + category.ReservedBytes);
    }
}
