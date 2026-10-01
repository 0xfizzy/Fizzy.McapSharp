using Xunit;
namespace Fizzy.McapSharp.Tests;
public class BatchSeekTests
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void GroupedSeekKeepsDuplicatesAndLeases(McapCompression compression)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 1024, CompressionThreads = 0 }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            for (uint i = 0; i < 2; i++)
            {
                var payload = new byte[70000];
                Array.Fill(payload, (byte)i);
                writer.WriteMessage(new(channel, i, i, 0), payload);
            }
            writer.Complete();
        }
        using var snapshot = new McapIndexSnapshot(output.ToArray(), new());
        var chunks = snapshot.GetSummary()!.ChunkIndexes;
        using var a = new McapPreparedChunkIndex(chunks[0]);
        using var b = new McapPreparedChunkIndex(chunks[1]);
        var ea = snapshot.ReadMessageIndexes(a)[0].Records[0];
        var eb = snapshot.ReadMessageIndexes(b)[0].Records[0];
        // Cross both the 64 KiB ordering page and the smaller message-slot pages.
        var requests = new McapSeekRequest[8201];
        for (int i = 0; i < requests.Length; i++)
            requests[i] = (i % 3 == 0) ? new(b, eb) : new(a, ea);
        using var batch = snapshot.SeekMessages(requests);
        Assert.Equal(2UL, snapshot.GetCacheStatistics().ChunkLoads);
        snapshot.Dispose();
        for (int i = 0; i < requests.Length; i++)
        {
            var expected = (uint)(i % 3 == 0 ? 1 : 0);
            Assert.Equal(expected, batch.GetHeader(i).Sequence);
            Assert.Equal((byte)expected, batch.GetPayload(i)[0]);
        }

        batch.Dispose();
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
