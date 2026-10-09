using Xunit;
namespace Fizzy.McapSharp.Tests;
public class LeaseTests
{

    [Fact]
    public void IndependentRetainsReleaseConcurrentlyWithoutInvalidatingOwner()
    {
        using var reader = new McapReadCursor(DeliveryOptimizationTests.Recording(McapCompression.Lz4), McapCursorMode.Messages, false);
        using var batch = reader.ReadBatchLease()!;
        reader.Dispose();
        Parallel.For(0, 1024, _ =>
        {
            using var retained = batch.RetainMessage(0);
            Assert.Equal(batch.GetHeader(0), retained.Header);
            Assert.True(retained.Payload.SequenceEqual(batch.GetPayload(0)));
        });

        Assert.Equal(70000, batch.GetPayload(0).Length);
        batch.Dispose();
        batch.Dispose();
    }

    [Fact]
    public void AbandonedLeaseSafeHandleReleasesStorage()
    {
        var abandoned = CreateAbandonedLease();
        for (int attempt = 0; attempt < 5 && abandoned.IsAlive; attempt++)
        {
            GC.Collect();
            GC.WaitForPendingFinalizers();
            GC.Collect();
        }
        Assert.False(abandoned.IsAlive);
    }

    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    static WeakReference CreateAbandonedLease()
    {
        using var reader = new McapReadCursor(DeliveryOptimizationTests.Recording(McapCompression.Lz4), McapCursorMode.Messages, false);
        var batch = reader.ReadBatchLease()!;
        var retained = batch.RetainMessage(0);
        batch.Dispose();
        reader.Dispose();

        return new WeakReference(retained);
    }

    [Theory]
    [InlineData(McapCompression.None,1)] [InlineData(McapCompression.None,8)] [InlineData(McapCompression.None,32)]
    [InlineData(McapCompression.Lz4,1)] [InlineData(McapCompression.Lz4,8)] [InlineData(McapCompression.Lz4,32)]
    [InlineData(McapCompression.Zstd,1)] [InlineData(McapCompression.Zstd,8)] [InlineData(McapCompression.Zstd,32)]
    public async Task LargeAsyncPayloadSurvivesShortReads(McapCompression compression,int mib)
    {
        var payload=new byte[mib*1024*1024];new Random(1234).NextBytes(payload);
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){Compression=compression,CompressionThreads=0},true)) {
            var c=writer.RegisterChannel("t","raw");writer.WriteMessage(new(c,1,1,0),payload);writer.Complete();
        }
        stream.Position=0;
        using var reader=new McapAsyncReader(stream,new(){},true,4096);
        using var batch=await reader.ReadBatchLeaseAsync(1);
        Assert.NotNull(batch);
        Assert.True(batch.GetPayload(0).SequenceEqual(payload));

    }

    [Theory]
    [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void TenThousandBatchesPreserveRetainedMessages(McapCompression compression)
    {
        using var stream=new MemoryStream();
        var payload=new byte[4096];
        new Random(123).NextBytes(payload);
        using(var writer=new McapWriter(stream,new(){Compression=compression,ChunkSize=1024,CompressionThreads=0},true)) {
            var c=writer.RegisterChannel("t","raw");
            for(uint i=0;i<10000;i++) writer.WriteMessage(new(c,i,i,0),payload);
            writer.Complete();
        }
        stream.Position=0;
        using var reader=McapReaderFactory.OpenMessages(stream,new(){Order=McapReadOrder.File},true,new(){});
        var retained=new Queue<McapMessageLease>();
        for(uint i=0;i<10000;i++) {
            using var batch=reader.ReadBatchLease(1)!;
            Assert.Equal(i,batch.GetHeader(0).Sequence);
            retained.Enqueue(batch.RetainMessage(0));
            if(retained.Count>4) { using var old=retained.Dequeue(); Assert.True(old.Payload.SequenceEqual(payload)); }

        }
        Assert.Null(reader.ReadBatchLease(1));reader.Dispose();
        while(retained.TryDequeue(out var lease)) { Assert.True(lease.Payload.SequenceEqual(payload)); lease.Dispose(); }

    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void LeasesSurviveAdvancementAndReaderDisposal(McapCompression compression)
    {
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){Compression=compression,ChunkSize=32768},true))
        {
            var c=writer.RegisterChannel("t","raw");
            for(uint i=0;i<20;i++) { var bytes=new byte[65536]; Array.Fill(bytes,(byte)i); writer.WriteMessage(new(c,i,i,0),bytes); }
            writer.Complete();
        }
        foreach(bool indexed in new[]{false,true})
        {
            stream.Position=0;
            using var reader=McapReaderFactory.OpenMessages(stream,new(){Order=indexed?McapReadOrder.LogTime:McapReadOrder.File},true);
            var leases=new List<McapMessageBatchLease>();
            for(int i=0;i<20;i++) leases.Add(reader.ReadBatchLease(1)!);
            Assert.Null(reader.ReadBatchLease()); reader.Dispose();
            using var retained=leases[0].RetainMessage(0);
            for(int i=0;i<20;i++)
            {
                Assert.Equal((uint)i,leases[i].GetHeader(0).Sequence);
                Assert.Equal(65536,leases[i].GetPayload(0).Length);
                foreach(byte b in leases[i].GetPayload(0)) Assert.Equal((byte)i,b);
                leases[i].Dispose();
                Assert.Throws<ObjectDisposedException>(()=>leases[i].GetHeader(0));
            }
            Assert.Equal(65536,retained.Payload.Length);
            Assert.Equal(0,retained.Payload[0]);
        }
    }
    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void MappedBorrowingPreservesPayloads(McapCompression compression)
    {
        var path=Path.Combine(Path.GetTempPath(),Guid.NewGuid()+".mcap");
        File.WriteAllBytes(path,DeliveryOptimizationTests.Recording(compression));
        try
        {
            using var reader=McapReadCursor.OpenMapped(path);
            using var batch=reader.ReadBatchLease()!;
            Assert.Equal(2,batch.Count);

            // Schema (s/raw/one byte) and channel (t0/raw/k/v), repeated in summary.

            reader.Dispose();
            Assert.Equal(70000,batch.GetPayload(0).Length);
        }
        finally { File.Delete(path); }
    }
}
