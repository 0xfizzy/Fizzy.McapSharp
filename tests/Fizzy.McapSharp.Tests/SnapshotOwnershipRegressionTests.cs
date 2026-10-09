using Xunit;

namespace Fizzy.McapSharp.Tests;

public class SnapshotOwnershipRegressionTests
{
    static byte[] Recording(int attachmentBytes = 0, McapCompression compression = McapCompression.None)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, ChunkSize = 1024 * 1024 }, true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            writer.WriteMessage(new(channel, 0, 1, 1), [11]);
            writer.Flush(); // Explicitly finish the first message chunk, including its declaration.
            writer.WriteMessage(new(channel, 1, 2, 2), [22]);
            if (attachmentBytes != 0) writer.WriteAttachment("attachment", "raw", 1, 2, new byte[attachmentBytes]);
            writer.Complete();
        }
        return stream.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void PreparedBorrowedAndCallerBufferReadsRemainAllocationFree(McapCompression compression)
    {
        using var snapshot = new McapIndexSnapshot(Recording(compression: compression));
        using var index = new McapPreparedChunkIndex(snapshot.GetSummary()!.ChunkIndexes[0]);
        var entry = snapshot.ReadMessageIndexes(index)[0].Records[0];
        var payload = new byte[1];
        var packedIndex = new byte[18];
        int visits = 0;
        McapMessageVisitor visitor = (in McapMessageHeader header, ReadOnlySpan<byte> data) => { visits += data[0] == 11 ? 1 : 0; return true; };
        void Read()
        {
            snapshot.SeekMessage(index, entry, visitor);
            snapshot.SeekMessage(index, entry, payload, out _, out _);
            snapshot.ReadMessageIndexes(index, packedIndex, out _);
        }
        for (int i = 0; i < 128; i++) Read();
        long before = GC.GetAllocatedBytesForCurrentThread();
        for (int i = 0; i < 128; i++) Read();
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
        Assert.Equal(0, allocated);
        Assert.Equal(256, visits);
        Assert.Equal(11, payload[0]);
    }

    [Fact]
    public void IndexedAttachmentCopiesOnlyIntoIndependentFinalPayload()
    {
        const int size = 8 * 1024 * 1024;
        using var snapshot = new McapIndexSnapshot(Recording(size));
        var index = snapshot.GetSummary()!.AttachmentIndexes.Single();
        _ = snapshot.ReadAttachment(index);
        long before = GC.GetAllocatedBytesForCurrentThread();
        var attachment = snapshot.ReadAttachment(index);
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
        // Allow cold control objects/strings; reject a second payload-sized body array.
        Assert.InRange(allocated, size, size + 64 * 1024);
        attachment.Data[0] = 123;
        var independent = snapshot.ReadAttachment(index);
        snapshot.Dispose();
        Assert.Equal(123, attachment.Data[0]);
        Assert.Equal(0, independent.Data[0]);
        Assert.Equal(size, independent.Data.Length);
    }

    [Fact]
    public async Task ReversedBatchDescriptorOrderAcrossSnapshotsCompletes()
    {
        var bytes = Recording();
        using var first = new McapIndexSnapshot(bytes);
        using var second = new McapIndexSnapshot(bytes);
        var chunks = first.GetSummary()!.ChunkIndexes;
        Assert.Equal(2, chunks.Count);
        using var a = new McapPreparedChunkIndex(chunks[0]);
        using var b = new McapPreparedChunkIndex(chunks[1]);
        var ea = first.ReadMessageIndexes(a)[0].Records[0];
        var eb = first.ReadMessageIndexes(b)[0].Records[0];
        using var start = new Barrier(2);
        Task Run(McapIndexSnapshot snapshot, McapSeekRequest[] requests) => Task.Run(() =>
        {
            start.SignalAndWait();
            for (int i = 0; i < 100; i++)
            {
                using var batch = snapshot.SeekMessages(requests);
                Assert.Equal(2, batch.Count);
            }
        });
        await Task.WhenAll(Run(first, [new(a, ea), new(b, eb)]), Run(second, [new(b, eb), new(a, ea)])).WaitAsync(TimeSpan.FromSeconds(15));
    }

    [Fact]
    public void BatchAndDisposeBothWaitForDescriptorSerialization()
    {
        using var snapshot = new McapIndexSnapshot(Recording());
        using var index = new McapPreparedChunkIndex(snapshot.GetSummary()!.ChunkIndexes[0]);
        var entry = snapshot.ReadMessageIndexes(index)[0].Records[0];
        // Hold the actual lifetime gate so both public operations reach a known contention
        // point, without depending on native operation duration or a timed sleep.
        var gate = typeof(McapPreparedChunkIndex).GetField("Gate", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!.GetValue(index)!;
        using var batchStarted = new ManualResetEventSlim();
        using var disposeStarted = new ManualResetEventSlim();
        using var batchDone = new ManualResetEventSlim();
        using var disposeDone = new ManualResetEventSlim();
        Exception? batchError = null, disposeError = null;
        var batchThread = new Thread(() =>
        {
            batchStarted.Set();
            try { using var batch = snapshot.SeekMessages([new(index, entry)]); Assert.Equal(11, batch.GetPayload(0)[0]); }
            catch (Exception e) { batchError = e; }
            finally { batchDone.Set(); }
        }) { IsBackground = true };
        var disposeThread = new Thread(() =>
        {
            disposeStarted.Set();
            try { index.Dispose(); }
            catch (Exception e) { disposeError = e; }
            finally { disposeDone.Set(); }
        }) { IsBackground = true };
        bool batchJoined = false, disposeJoined = false;
        Monitor.Enter(gate);
        try
        {
            batchThread.Start();
            disposeThread.Start();
            Assert.True(batchStarted.Wait(TimeSpan.FromSeconds(5)));
            Assert.True(disposeStarted.Wait(TimeSpan.FromSeconds(5)));
            Assert.True(SpinWait.SpinUntil(() => batchDone.IsSet || (batchThread.ThreadState & ThreadState.WaitSleepJoin) != 0, TimeSpan.FromSeconds(5)));
            Assert.True(SpinWait.SpinUntil(() => disposeDone.IsSet || (disposeThread.ThreadState & ThreadState.WaitSleepJoin) != 0, TimeSpan.FromSeconds(5)));
            Assert.False(batchDone.IsSet);
            Assert.False(disposeDone.IsSet);
        }
        finally
        {
            Monitor.Exit(gate);
            if ((batchThread.ThreadState & ThreadState.Unstarted) == 0) batchJoined = batchThread.Join(TimeSpan.FromSeconds(5));
            if ((disposeThread.ThreadState & ThreadState.Unstarted) == 0) disposeJoined = disposeThread.Join(TimeSpan.FromSeconds(5));
        }
        Assert.True(batchJoined);
        Assert.True(disposeJoined);
        Assert.Null(disposeError);
        // Either operation may win after the gate is released; no native pointer may
        // escape descriptor disposal, and a successful lease has independent storage.
        if (batchError is not null) Assert.IsType<ObjectDisposedException>(batchError);
    }

    [Fact]
    public async Task ConcurrentSnapshotCallbacksRejectCrossSnapshotCallsWithoutWaitingForLocks()
    {
        var bytes = Recording();
        var first = new McapIndexSnapshot(bytes);
        var second = new McapIndexSnapshot(bytes);
        var a = new McapPreparedChunkIndex(first.GetSummary()!.ChunkIndexes[0]);
        var b = new McapPreparedChunkIndex(second.GetSummary()!.ChunkIndexes[0]);
        var ea = first.ReadMessageIndexes(a)[0].Records[0];
        var eb = second.ReadMessageIndexes(b)[0].Records[0];
        var callbacksEntered = new Barrier(2);
        Task Run(McapIndexSnapshot source, McapPreparedChunkIndex index, McapMessageIndexEntry entry, McapIndexSnapshot other) => Task.Run(() =>
        {
            source.SeekMessage(index, entry, (in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
            {
                Assert.True(callbacksEntered.SignalAndWait(TimeSpan.FromSeconds(5)));
                Assert.Throws<InvalidOperationException>(() => other.GetSummary());
                Assert.Throws<InvalidOperationException>(() => other.GetCacheStatistics());
                Assert.Throws<InvalidOperationException>(() => other.ReadFooter());
                Assert.Throws<InvalidOperationException>(() => other.Dispose());
                return true;
            });
        });
        var calls = Task.WhenAll(Run(first, a, ea, second), Run(second, b, eb, first));
        try
        {
            await calls.WaitAsync(TimeSpan.FromSeconds(15));
            // Failed reentry must leave the callback scope and both snapshots usable.
            Assert.NotNull(first.GetSummary());
            Assert.NotNull(second.GetSummary());
        }
        finally
        {
            // A deadlock regression must fail this test within the deadline, rather than
            // blocking the test runner again while trying to dispose a locked snapshot.
            if (calls.IsCompleted) { callbacksEntered.Dispose(); a.Dispose(); b.Dispose(); first.Dispose(); second.Dispose(); }
        }
    }

    [Fact]
    public void SnapshotCallbackRejectsPreparedReentryBeforeTakingLocks()
    {
        var bytes = Recording();
        using var first = new McapIndexSnapshot(bytes);
        using var second = new McapIndexSnapshot(bytes);
        var chunk = first.GetSummary()!.ChunkIndexes[0];
        using var index = new McapPreparedChunkIndex(chunk);
        var entry = first.ReadMessageIndexes(index)[0].Records[0];
        bool Visit(in McapMessageHeader header, ReadOnlySpan<byte> payload)
        {
            Assert.Throws<InvalidOperationException>(() => index.Dispose());
            Assert.Throws<InvalidOperationException>(() => second.SeekMessage(index, entry));
            Assert.Throws<InvalidOperationException>(() => second.SeekMessages([new(index, entry)]));
            Assert.Throws<InvalidOperationException>(() => second.ReadMessageIndexes(index));
            Assert.Throws<InvalidOperationException>(() => second.GetSummary());
            Assert.Throws<InvalidOperationException>(() => second.Dispose());
            Assert.Throws<InvalidOperationException>(() => { using var messages = second.ReadChunkMessages(chunk).GetEnumerator(); messages.MoveNext(); });
            return true;
        }
        first.SeekMessage(index, entry, Visit);
        using var batch = second.SeekMessages([new(index, entry)]);
        Assert.Equal(11, batch.GetPayload(0)[0]);
        index.Dispose();
        Assert.Throws<ObjectDisposedException>(() => first.SeekMessages([new(index, entry)]));
    }
}
