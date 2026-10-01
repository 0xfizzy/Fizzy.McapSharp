using Xunit;
namespace Fizzy.McapSharp.Tests;
public class LeaseTests
{
    [Fact]
    public async Task UnrelatedSmallLeaseCannotMakeAnImpossibleReaderWaitForever()
    {
        static byte[] Recording(int size)
        {
            using var output=new MemoryStream();
            using(var writer=new McapWriter(output,new(){UseChunks=false,Compression=McapCompression.None},true))
            {
                var channel=writer.RegisterChannel("t","raw");
                writer.WriteMessage(new(channel,0,0,0),new byte[size]);
                writer.Complete();
            }
            return output.ToArray();
        }
        var data=Recording(70000);
        var probeBudget=new McapMemoryBudget(maxBlockBytes:80000,maxRetainedBytes:0);
        using(var source=new MemoryStream(data))
        using(var probe=new McapAsyncReader(source,new(){Memory=new(){Budget=probeBudget}},true))
        using(var batch=await probe.ReadBatchLeaseAsync()) Assert.Single(Enumerable.Range(0,batch!.Count));
        ulong limit=probeBudget.GetStatistics().PeakBytes-8192;
        Assert.True(limit>80000);
        var budget=new McapMemoryBudget(limit,80000,0);
        McapMessageBatchLease small;
        using(var reader=new McapBufferReader(Recording(1),McapBufferReadMode.Messages,false,new(){Budget=budget}))
            small=reader.ReadBatchLease(1)!;
        using(small)
        {
            Assert.InRange(budget.GetStatistics().CurrentBytes,1UL,8191UL);
            using var source=new MemoryStream(data);
            using var reader=new McapAsyncReader(source,new(){Memory=new(){Budget=budget}},true);
            using var deadline=new CancellationTokenSource(TimeSpan.FromSeconds(3));
            await Assert.ThrowsAsync<McapException>(async()=>await reader.ReadBatchLeaseAsync(cancellationToken:deadline.Token));
            Assert.Equal(1,small.GetPayload(0).Length);
        }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void IndependentRetainsReleaseConcurrentlyWithoutInvalidatingOwner()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var reader = new McapBufferReader(DeliveryOptimizationTests.Recording(McapCompression.Lz4), McapBufferReadMode.Messages, false,
            options: new() { Budget = budget });
        using var batch = reader.ReadBatchLease()!;
        reader.Dispose();
        var before = budget.GetStatistics().CurrentBytes;
        Parallel.For(0, 1024, _ =>
        {
            using var retained = batch.RetainMessage(0);
            Assert.Equal(batch.GetHeader(0), retained.Header);
            Assert.True(retained.Payload.SequenceEqual(batch.GetPayload(0)));
        });
        Assert.Equal(before, budget.GetStatistics().CurrentBytes);
        Assert.Equal(70000, batch.GetPayload(0).Length);
        batch.Dispose();
        batch.Dispose();
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void AbandonedLeaseSafeHandleReleasesStorage()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        var abandoned = CreateAbandonedLease(budget);
        for (int attempt = 0; attempt < 5 && budget.GetStatistics().CurrentBytes != BudgetAssertions.ControlBytes; attempt++)
        {
            GC.Collect();
            GC.WaitForPendingFinalizers();
            GC.Collect();
        }
        Assert.False(abandoned.IsAlive);
        BudgetAssertions.Idle(budget);
    }

    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    static WeakReference CreateAbandonedLease(McapMemoryBudget budget)
    {
        using var reader = new McapBufferReader(DeliveryOptimizationTests.Recording(McapCompression.Lz4), McapBufferReadMode.Messages, false,
            options: new() { Budget = budget });
        var batch = reader.ReadBatchLease()!;
        var retained = batch.RetainMessage(0);
        batch.Dispose();
        reader.Dispose();
        Assert.True(budget.GetStatistics().CurrentBytes > 0);
        return new WeakReference(retained);
    }

    [Fact]
    public void WriterDeclarationsUseTheSharedFiniteDomain()
    {
        // Include the charged constructor document peak before testing later declaration pressure.
        var probeBudget=new McapMemoryBudget(maxRetainedBytes:0);
        using var probeStream=new MemoryStream();
        ulong initializationPeak;
        using(var probe=new McapWriter(probeStream,new(){UseChunks=false,Memory=new(){Budget=probeBudget}},true))
            initializationPeak=probeBudget.GetStatistics().PeakBytes;
        var budget=new McapMemoryBudget(initializationPeak+8192,4096,0);
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){UseChunks=false,Memory=new(){Budget=budget}},true)) {
            var error=Assert.Throws<McapException>(()=> {for(int i=0;i<1000;i++) writer.RegisterChannel("channel-"+i,"raw");});
            Assert.False(error.CanContinueWriting);
            Assert.InRange(budget.GetStatistics().PeakBytes,1UL,budget.MaxBytes);
            Assert.Throws<InvalidOperationException>(()=>writer.Complete());
        }
        BudgetAssertions.Idle(budget);
    }

    [Theory]
    [InlineData(McapCompression.None,1)] [InlineData(McapCompression.None,8)] [InlineData(McapCompression.None,32)]
    [InlineData(McapCompression.Lz4,1)] [InlineData(McapCompression.Lz4,8)] [InlineData(McapCompression.Lz4,32)]
    [InlineData(McapCompression.Zstd,1)] [InlineData(McapCompression.Zstd,8)] [InlineData(McapCompression.Zstd,32)]
    public async Task LargeAsyncPayloadDoesNotRepeatedlyCopyShortReadPrefixes(McapCompression compression,int mib)
    {
        var payload=new byte[mib*1024*1024];new Random(1234).NextBytes(payload);
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){Compression=compression,CompressionThreads=0},true)) {
            var c=writer.RegisterChannel("t","raw");writer.WriteMessage(new(c,1,1,0),payload);writer.Complete();
        }
        stream.Position=0;
        var budget=new McapMemoryBudget();
        using var reader=new McapAsyncReader(stream,new(){Memory=new(){Budget=budget}},true,4096);
        using var batch=await reader.ReadBatchLeaseAsync(1);
        Assert.NotNull(batch);
        Assert.True(batch.GetPayload(0).SequenceEqual(payload));
        Assert.InRange(budget.GetStatistics().StorageCopyBytes,0UL,65536UL);
        Assert.InRange(budget.GetStatistics().PeakBytes,0UL,budget.MaxBytes);
    }
    [Fact]
    public async Task ImpossibleAsyncWorkingSetFailsWithoutWaitingForNonexistentLease()
    {
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){Compression=McapCompression.Lz4},true)) {
            var c=writer.RegisterChannel("t","raw");writer.WriteMessage(new(c,1,1,0),new byte[256*1024]);writer.Complete();
        }
        stream.Position=0;
        var probeBudget=new McapMemoryBudget(maxRetainedBytes:0);
        ulong constructorPeak;
        using(var probe=new McapAsyncReader(stream,new(){Memory=new(){Budget=probeBudget}},true))
            constructorPeak=probeBudget.GetStatistics().PeakBytes;
        using var reader=new McapAsyncReader(stream,new(){Memory=new(){Budget=new(Math.Max(constructorPeak+8500,270*1024),270*1024,0)}},true);
        await Assert.ThrowsAsync<McapException>(()=>reader.ReadBatchLeaseAsync(1).AsTask().WaitAsync(TimeSpan.FromSeconds(5)));
    }

    [Theory]
    [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void TenThousandBatchesRemainBoundedWithRetainedMessages(McapCompression compression)
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
        var budget=new McapMemoryBudget(64*1024*1024,16*1024,32*1024);
        using var reader=McapReader.OpenMessages(stream,new(){Order=McapReadOrder.File},true,new(){Memory=new(){Budget=budget}});
        var retained=new Queue<McapMessageLease>();
        for(uint i=0;i<10000;i++) {
            using var batch=reader.ReadBatchLease(1)!;
            Assert.Equal(i,batch.GetHeader(0).Sequence);
            retained.Enqueue(batch.RetainMessage(0));
            if(retained.Count>4) { using var old=retained.Dequeue(); Assert.True(old.Payload.SequenceEqual(payload)); }
            Assert.InRange(budget.GetStatistics().CurrentBytes,0UL,budget.MaxBytes);
        }
        Assert.Null(reader.ReadBatchLease(1));reader.Dispose();
        while(retained.TryDequeue(out var lease)) { Assert.True(lease.Payload.SequenceEqual(payload)); lease.Dispose(); }
        Assert.InRange(budget.GetStatistics().CurrentBytes,0UL,budget.MaxRetainedBytes);
    }

    [Fact]
    public async Task AsyncLeasesWaitForReleaseAndRequireConsumption()
    {
        using var stream=new MemoryStream();
        using(var writer=new McapWriter(stream,new(){ChunkSize=1024},true)) {
            var c=writer.RegisterChannel("t","raw");
            for(uint i=0;i<3;i++) writer.WriteMessage(new(c,i,i,0),new byte[70000]);
            writer.Complete();
        }
        stream.Position=0;
        ulong capacity;
        var probeBudget=new McapMemoryBudget(maxBlockBytes:80000,maxRetainedBytes:0);
        using(var probe=new McapAsyncReader(stream,new(){Memory=new(){Budget=probeBudget}},true)) {
            using var sample=await probe.ReadBatchLeaseAsync(1);
            capacity=probeBudget.GetStatistics().PeakBytes+8192;
        }
        stream.Position=0;
        var budget=new McapMemoryBudget(capacity,80000,0);
        using var reader=new McapAsyncReader(stream,new(){Memory=new(){Budget=budget}},true);
        using var first=await reader.ReadBatchLeaseAsync(1);
        Assert.NotNull(first);
        var pending=reader.ReadBatchLeaseAsync(1);
        Assert.False(pending.IsCompleted);
        Assert.Throws<InvalidOperationException>(()=>reader.ReadBatchLeaseAsync(1));
        Assert.Throws<InvalidOperationException>(()=>reader.Dispose());
        first.Dispose();
        using var second=await pending.AsTask().WaitAsync(TimeSpan.FromSeconds(10));
        Assert.Equal(1U,second!.GetHeader(0).Sequence);
        using var cancel=new CancellationTokenSource();
        var blocked=reader.ReadBatchLeaseAsync(1,cancellationToken:cancel.Token);
        Assert.False(blocked.IsCompleted);
        cancel.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>blocked.AsTask());
        Assert.Equal(70000,second.GetPayload(0).Length);
        Assert.Throws<InvalidOperationException>(()=>reader.ReadBatchLeaseAsync(1));
    }
    [Theory]
    [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void OccupiedBudgetCanRetryWithoutSkipping(McapCompression compression)
    {
        var path=Path.Combine(Path.GetTempPath(),Guid.NewGuid()+".mcap");
        try
        {
            using(var writer=new McapWriter(path,new(){Compression=compression,ChunkSize=1024}))
            {
                var c=writer.RegisterChannel("t","raw");
                writer.WriteMessage(new(c,1,1,0),new byte[70000]);
                writer.WriteMessage(new(c,2,2,0),new byte[70000]);
                writer.Complete();
            }
            var probeBudget=new McapMemoryBudget(maxBlockBytes:80000,maxRetainedBytes:0);
            ulong capacity;
            using(var probe=new McapReader(path).OpenMessages(new(){Order=McapReadOrder.File},new(){Memory=new(){Budget=probeBudget}})) {
                using var sample=probe.ReadBatchLease(1);
                capacity=probeBudget.GetStatistics().PeakBytes+8192;
            }
            var budget=new McapMemoryBudget(capacity,80000,0);
            using var reader=new McapReader(path).OpenMessages(new(){Order=McapReadOrder.File},new(){Memory=new(){Budget=budget}});
            using var first=reader.ReadBatchLease(1)!;
            Assert.Equal(1U,first.GetHeader(0).Sequence);
            Assert.Equal(McapLeaseReadStatus.BudgetUnavailable,reader.TryReadBatchLease(out var unavailable,1));
            Assert.Null(unavailable);
            Assert.InRange(budget.GetStatistics().CurrentBytes,70000UL,budget.MaxBytes);
            first.Dispose();
            using var second=reader.ReadBatchLease(1)!;
            Assert.Equal(2U,second.GetHeader(0).Sequence);
            Assert.InRange(budget.GetStatistics().PeakBytes,70000UL,budget.MaxBytes);
        }
        finally { File.Delete(path); }
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
            using var reader=McapReader.OpenMessages(stream,new(){Order=indexed?McapReadOrder.LogTime:McapReadOrder.File},true);
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
    public void MappedBorrowingHasNoWrapperPayloadCopies(McapCompression compression)
    {
        var path=Path.Combine(Path.GetTempPath(),Guid.NewGuid()+".mcap");
        File.WriteAllBytes(path,DeliveryOptimizationTests.Recording(compression));
        try
        {
            var budget=new McapMemoryBudget();
            using var reader=McapBufferReader.OpenMapped(path,options:new(){Budget=budget});
            using var batch=reader.ReadBatchLease()!;
            Assert.Equal(2,batch.Count);
            Assert.Equal(0UL,reader.GetMemoryStatistics().CopiedBytes);
            var flow = budget.GetDetailedStatistics().Flow;
            Assert.Equal(0UL, flow.InputCopyBytes);
            Assert.Equal(0UL, flow.CompactionCopyBytes);
            Assert.Equal(0UL, flow.DeliveryCopyBytes);
            // Schema (s/raw/one byte) and channel (t0/raw/k/v), repeated in summary.
            Assert.Equal(2UL * (1 + 3 + 1 + 2 + 3 + 1 + 1), flow.OtherCopyBytes);
            Assert.Equal(flow.OtherCopyBytes, budget.GetStatistics().StorageCopyBytes);
            reader.Dispose();
            Assert.Equal(70000,batch.GetPayload(0).Length);
        }
        finally { File.Delete(path); }
    }
}
