using Fizzy.McapSharp;
using System.Threading.Tasks.Sources;

static class Extended
{
    public static void Run()
    {
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
            using var storage = new MemoryStream(32 * 1024 * 1024);
            using var channel = new McapPreparedChannel(new(17, "full", "raw", new(19, "s", "raw", [1]), new Dictionary<string, string>()));
            using var late = new McapPreparedChannel(new(18, "late", "raw", new(22, "late-schema", "raw", [2]), new Dictionary<string, string>()));
            using var metadata = McapPreparedOperation.Metadata("m", new Dictionary<string, string> { ["a"] = "b" });
            using var attachment = McapPreparedOperation.Attachment("a", "raw", 1, 2);
            using var start = McapPreparedOperation.StartAttachment("b", "raw", 1, 2, 1);
            using var schema = McapPreparedOperation.Schema("schema", "raw", [1], 20);
            using var declaration = McapPreparedOperation.Channel("declared", "raw", 20, id: 21);
            using (var w = new McapWriter(storage, new() { Compression = compression, ChunkSize = 4096, CompressionThreads = 0 }, true))
            {
                for (int i = 0; i < 100; i++) Write(w, channel, metadata, attachment, start, schema, declaration);
                long before = GC.GetAllocatedBytesForCurrentThread();
                w.WriteMessage(late, new(18, 0, 0, 0), []);
                for (int i = 0; i < 300; i++) Write(w, channel, metadata, attachment, start, schema, declaration);
                long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
                Check("full-message/control/late-declaration " + compression, allocated);
                w.Complete();
            }
            var bytes = storage.ToArray();
            using (var r = new McapReadCursor(bytes, McapCursorMode.ExpandedRecords))
            {
                byte[] buffer = new byte[65536];
                for (int i = 0; i < 100; i++) { r.ReadNextRecord(buffer, out var op, out var size); var v = McapRecordView.Parse(op, buffer.AsSpan(0, (int)size)); if (v.Opcode == 5) _ = v.MessageHeader; }
                var before = GC.GetAllocatedBytesForCurrentThread();
                while (r.ReadNextRecord(buffer, out var opcode, out var n) != McapReadStatus.EndOfStream)
                {
                    var view = McapRecordView.Parse(opcode, buffer.AsSpan(0, (int)n));
                    if (view.Opcode == 5) _ = view.MessageHeader;
                }
                long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
                Check("buffer/record-view " + compression, allocated);
            }
            storage.Position = 0;
            using (var r = McapReaderFactory.OpenMessages(storage, leaveOpen: true))
            using (var snapshot = r.OpenIndexSnapshot())
            {
                var summary = r.GetSummary()!;
                var chunk = summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count != 0);
                var entry = r.ReadMessageIndexes(chunk)[0].Records[0];
                // Exercise interface-only maps and indexes larger than the stack buffer.
                var alternate = chunk with
                {
                    MessageIndexOffsets = new System.Collections.ObjectModel.ReadOnlyDictionary<ushort, ulong>(new Dictionary<ushort, ulong>(chunk.MessageIndexOffsets)),
                    Compression = new string('x', 2048)
                };
                byte[] buffer = new byte[65536];
                for (int i = 0; i < 10; i++) { Random(snapshot, chunk, entry, summary, buffer); snapshot.ReadMessageIndexes(alternate, buffer, out _); }
                long before = GC.GetAllocatedBytesForCurrentThread();
                for (int i = 0; i < 50; i++) { Random(snapshot, chunk, entry, summary, buffer); snapshot.ReadMessageIndexes(alternate, buffer, out _); }
                long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
                Check("random/index/attachment/metadata " + compression, allocated);
                using var cursor = snapshot.OpenChunkReader(chunk);
                cursor.ReadNext([], out _, out _);
                before = GC.GetAllocatedBytesForCurrentThread();
                while (cursor.ReadNext(buffer, out _, out _) != McapReadStatus.EndOfStream) { }
                cursor.ReadNext(buffer, out _, out _);
                allocated = GC.GetAllocatedBytesForCurrentThread() - before;
                Check("independent lazy chunk " + compression, allocated);
            }
            Async(bytes, compression);
            AsyncAwait(bytes, compression);
        }
    }
    static void Write(McapWriter w, McapPreparedChannel c, McapPreparedOperation m, McapPreparedOperation a, McapPreparedOperation start, McapPreparedOperation schema, McapPreparedOperation channel)
    {
        w.WriteMessage(c, new(17, 1, 1, 1), [1]);
        w.WritePrepared(schema); w.WritePrepared(channel); w.WritePrepared(m); w.WritePrepared(a, [1]);
        w.WritePrepared(start); w.WriteAttachmentBytes([1]); w.FinishAttachment(); w.WritePrivateRecord(0x80, [1], true);
    }
    static void Random(McapIndexSnapshot s, McapChunkIndex chunk, McapMessageIndexEntry entry, McapSummary summary, byte[] buffer)
    {
        s.SeekMessage(chunk, entry, [], out _, out _); s.SeekMessage(chunk, entry, buffer, out _, out _);
        s.ReadMessageIndexes(chunk, buffer, out _); s.ReadMetadata(summary.MetadataIndexes[0], buffer, out _); s.ReadAttachment(summary.AttachmentIndexes[0], buffer, out _);
    }
    static void Async(byte[] bytes, McapCompression compression)
    {
        using var stream = new SuspendingStream(bytes);
        using var reader = new McapAsyncReader(stream, leaveOpen: true, inputBufferSize: 256);
        byte[] buffer = new byte[65536];
        for (int i = 0; i < 1000; i++) Read(reader, buffer);
        stream.WaitIdle();
        long worker = stream.Allocated;
        long main = GC.GetAllocatedBytesForCurrentThread();
        while (Read(reader, buffer).Status != McapReadStatus.EndOfStream) { }
        stream.WaitIdle();
        long allocated = GC.GetAllocatedBytesForCurrentThread() - main + stream.Allocated - worker;
        Check("async actual suspension (caller + worker) " + compression, allocated);
        if (stream.Suspensions == 0) throw new Exception("Async test did not suspend.");
    }
    static void AsyncAwait(byte[] bytes, McapCompression compression)
    {
        using var stream = new SuspendingStream(bytes);
        using var reader = new McapAsyncReader(stream, leaveOpen: true, inputBufferSize: 256);
        var buffer = new byte[65536];
        Consume().GetAwaiter().GetResult();
        stream.WaitIdle();
        Check("async direct await (I/O thread) " + compression, stream.Allocated);
        async Task Consume()
        {
            int count = 0;
            while ((await reader.ReadNextRecordAsync(buffer).ConfigureAwait(false)).Status != McapReadStatus.EndOfStream)
                if (++count == 1000) stream.ResetAllocated();
            if (count < 1000) throw new Exception("Insufficient async warmup");
        }
    }
    static McapRecordReadResult Read(McapAsyncReader reader, byte[] buffer)
    {
        var task = reader.ReadNextRecordAsync(buffer);
        var deadline = Environment.TickCount64 + 10000;
        while (!task.IsCompleted) { if (Environment.TickCount64 > deadline) throw new Exception("Async operation stalled"); Thread.SpinWait(50); Thread.Yield(); }
        return task.GetAwaiter().GetResult();
    }
    static void Check(string name, long bytes)
    {
        Console.WriteLine($"{name}: {bytes} B");
        if (bytes != 0) throw new Exception($"Allocation regression in {name}: {bytes}");
    }
}

sealed class SuspendingStream : Stream, IValueTaskSource<int>
{
    readonly byte[] data;
    readonly AutoResetEvent request = new(false);
    readonly ManualResetEventSlim idle = new(true);
    readonly Thread worker;
    ManualResetValueTaskSourceCore<int> completion;
    Memory<byte> destination;
    int position;
    readonly int maxRead;
    volatile bool stopped;
    public long Allocated;
    public int Suspensions;
    public void ResetAllocated() => Interlocked.Exchange(ref Allocated, 0);
    public SuspendingStream(byte[] data, int maxRead = int.MaxValue) { this.data = data; this.maxRead = maxRead; worker = new(Work); worker.Start(); }
    public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken token = default)
    {
        completion.Reset(); destination = buffer; idle.Reset();
        // Completion is dispatched only after continuation registration, guaranteeing suspension.
        return new(this, completion.Version);
    }
    void Work()
    {
        while (true)
        {
            request.WaitOne(); if (stopped) return;
            long before = GC.GetAllocatedBytesForCurrentThread();
            int n = Math.Min(maxRead, Math.Min(destination.Length, data.Length - position));
            data.AsMemory(position, n).CopyTo(destination); position += n;
            completion.SetResult(n);
            Interlocked.Add(ref Allocated, GC.GetAllocatedBytesForCurrentThread() - before);
            idle.Set();
        }
    }
    public void WaitIdle() { if (!idle.Wait(10000)) throw new Exception("Worker did not become idle"); }
    public int GetResult(short token) => completion.GetResult(token);
    public ValueTaskSourceStatus GetStatus(short token) => completion.GetStatus(token);
    public void OnCompleted(Action<object?> c, object? s, short t, ValueTaskSourceOnCompletedFlags f) { completion.OnCompleted(c, s, t, f); Interlocked.Increment(ref Suspensions); request.Set(); }
    protected override void Dispose(bool disposing) { stopped = true; request.Set(); if (!worker.Join(1000)) throw new Exception("Worker join timed out"); request.Dispose(); idle.Dispose(); base.Dispose(disposing); }
    public override bool CanRead => true; public override bool CanSeek => false; public override bool CanWrite => false;
    public override long Length => data.Length;
    public override long Position { get => position; set => throw new NotSupportedException(); }
    public override int Read(byte[] b, int o, int n) => throw new NotSupportedException();
    public override long Seek(long o, SeekOrigin s) => throw new NotSupportedException();
    public override void Write(byte[] b, int o, int n) => throw new NotSupportedException();
    public override void Flush() { }
    public override void SetLength(long n) => throw new NotSupportedException();
}
