using Xunit;
namespace Fizzy.McapSharp.Tests;
public class BatchSeekTests
{
    [Theory]
    [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void DomainPressureEvictsReusableChunksBeforeRejecting(McapCompression compression)
    {
        var path=Path.Combine(Path.GetTempPath(),Guid.NewGuid()+".mcap");
        try {
            using(var writer=new McapWriter(path,new(){Compression=compression,ChunkSize=1024,CompressionThreads=0})) {
                var c=writer.RegisterChannel("t","raw");
                for(uint i=0;i<2;i++) writer.WriteMessage(new(c,i,i,0),new byte[70000]);
                writer.Complete();
            }
            var budget=new McapMemoryBudget(180000,100000,0);
            using var snapshot=McapIndexSnapshot.OpenMapped(path,new(){Budget=budget,MaxRandomAccessCacheBytes=1024*1024});
            var indexes=snapshot.GetSummary()!.ChunkIndexes;
            using var a=new McapPreparedChunkIndex(indexes[0]);using var b=new McapPreparedChunkIndex(indexes[1]);
            var ea=snapshot.ReadMessageIndexes(a)[0].Records[0];var eb=snapshot.ReadMessageIndexes(b)[0].Records[0];
            McapMessageVisitor visitor=static (in McapMessageHeader h,ReadOnlySpan<byte> p)=>true;
            snapshot.SeekMessage(a,ea,visitor);snapshot.SeekMessage(b,eb,visitor);snapshot.SeekMessage(a,ea,visitor);
            Assert.Equal(3UL,snapshot.GetCacheStatistics().ChunkLoads);
            Assert.InRange(budget.GetStatistics().PeakBytes,0UL,budget.MaxBytes);
        }
        finally {File.Delete(path);}
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void ChunkLruAndGroupedSeekPreserveOrder(McapCompression compression)
    {
        using var output=new MemoryStream();
        using(var writer=new McapWriter(output,new(){Compression=compression,ChunkSize=1024},true)) {
            var c=writer.RegisterChannel("t","raw");
            for(uint i=0;i<4;i++) { var data=new byte[70000];Array.Fill(data,(byte)i);writer.WriteMessage(new(c,i,i,0),data); }
            writer.Complete();
        }
        using var snapshot=new McapIndexSnapshot(output.ToArray(),new(){MaxRandomAccessCacheBytes=1024*1024});
        var chunks=snapshot.GetSummary()!.ChunkIndexes;
        using var a=new McapPreparedChunkIndex(chunks[0]);using var b=new McapPreparedChunkIndex(chunks[1]);
        var ea=snapshot.ReadMessageIndexes(a)[0].Records[0];var eb=snapshot.ReadMessageIndexes(b)[0].Records[0];
        bool Visit(in McapMessageHeader h,ReadOnlySpan<byte> data) { Assert.Equal((byte)h.Sequence,data[0]);return false; }
        snapshot.SeekMessage(a,ea,Visit);snapshot.SeekMessage(b,eb,Visit);snapshot.SeekMessage(a,ea,Visit);
        Assert.Equal(new McapCacheStatistics(1,2),snapshot.GetCacheStatistics());
        using var batch=snapshot.SeekMessages([new(b,eb),new(a,ea),new(b,eb)]);
        Assert.Equal(1U,batch.GetHeader(0).Sequence);Assert.Equal(0U,batch.GetHeader(1).Sequence);Assert.Equal(1U,batch.GetHeader(2).Sequence);
        Assert.Equal(2UL,snapshot.GetCacheStatistics().ChunkLoads);
        snapshot.Dispose();Assert.Equal(70000,batch.GetPayload(0).Length);
        using var uncached=new McapIndexSnapshot(output.ToArray());
        using var grouped=uncached.SeekMessages([new(b,eb),new(a,ea),new(b,eb),new(a,ea)]);
        Assert.Equal(2UL,uncached.GetCacheStatistics().ChunkLoads);
        Assert.Equal(McapReadStatus.BufferTooSmall,uncached.SeekMessage(a,ea,Span<byte>.Empty,out _,out _));
        var loads=uncached.GetCacheStatistics().ChunkLoads;
        Assert.Equal(McapReadStatus.Message,uncached.SeekMessage(a,ea,new byte[70000],out _,out _));
        Assert.Equal(loads,uncached.GetCacheStatistics().ChunkLoads);
    }
}
