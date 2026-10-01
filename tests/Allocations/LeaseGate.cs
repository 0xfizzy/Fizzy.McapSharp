using System.Diagnostics;
using System.Text.Json;
using Fizzy.McapSharp;

// Fixed one-message batches separate permitted result allocations from I/O suspension costs.
static class LeaseGate
{
    const int Warmup = 64, Count = 128, PayloadSize = 8192;
    public static void Run(bool enforce)
    {
        _ = Stopwatch.GetTimestamp(); // Exclude the clock's cold initialization.
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
            byte[] file = Recording(compression);
            Measure(file, compression, 65536, false); // Warm process-wide first-use helpers as well.
            long baseline = Measure(file, compression, 65536, false).Allocated;
            foreach (int quantum in new[] { 65536, 4096, 512 })
            {
                var inline = Measure(file, compression, quantum, false);
                var suspended = Measure(file, compression, quantum, true);
                if (enforce && (inline.Allocated != baseline || suspended.Allocated != baseline))
                    throw new Exception($"Lease I/O allocated beyond result objects: {compression}/{quantum}: {baseline}, {inline.Allocated}, {suspended.Allocated}");
                if (suspended.Suspensions == 0) throw new Exception("Lease benchmark did not suspend.");
            }
            DirectAwait(file, compression);
        }
    }
    static byte[] Recording(McapCompression compression)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = compression, CompressionThreads = 0, ChunkSize = 32768 }, true))
        {
            var channel = writer.RegisterChannel("lease", "raw");
            byte[] payload = new byte[PayloadSize]; new Random(31).NextBytes(payload);
            for (uint i = 0; i < Warmup + Count; i++) writer.WriteMessage(new(channel, i, i, 0), payload);
            writer.Complete();
        }
        return output.ToArray();
    }
    static (long Allocated, int Suspensions) Measure(byte[] file, McapCompression compression, int quantum, bool suspend)
    {
        using Stream source = suspend ? new SuspendingStream(file, quantum) : new MemoryStream(file, false);
        var worker = source as SuspendingStream;
        using var reader = new McapAsyncReader(source, leaveOpen: true, inputBufferSize: quantum);
        for (int i = 0; i < Warmup; i++) { using var batch = Read(reader); }
        worker?.WaitIdle();
        var timings = new long[Count];
        long workerBefore = worker?.Allocated ?? 0;
        int suspensionsBefore = worker?.Suspensions ?? 0;
        long before = GC.GetAllocatedBytesForCurrentThread();
        long start = Stopwatch.GetTimestamp();
        for (int i = 0; i < Count; i++)
        {
            long tick = Stopwatch.GetTimestamp();
            using var batch = Read(reader);
            if (batch is null || batch.Count != 1 || batch.GetPayload(0).Length != PayloadSize)
                throw new Exception("Unexpected lease batch.");
            timings[i] = Stopwatch.GetTimestamp() - tick;
        }
        worker?.WaitIdle();
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before + (worker?.Allocated ?? 0) - workerBefore;
        double elapsed = Stopwatch.GetElapsedTime(start).TotalSeconds;
        int suspensions = (worker?.Suspensions ?? 0) - suspensionsBefore;
        Array.Sort(timings);
        Console.WriteLine(JsonSerializer.Serialize(new { kind = "async-lease", compression = compression.ToString(), quantum, suspend,
            batches = Count, messagesPerBatch = 1, allocated, suspensions, messagesPerSecond = Count / elapsed,
            medianBatchUs = timings[Count / 2] * 1e6 / Stopwatch.Frequency, p95BatchUs = timings[(int)(Count * .95)] * 1e6 / Stopwatch.Frequency }));
        return (allocated, suspensions);
    }
    static McapMessageBatchLease? Read(McapAsyncReader reader)
    {
        var task = reader.ReadBatchLeaseAsync(1);
        long deadline = Environment.TickCount64 + 10000;
        while (!task.IsCompleted)
        {
            if (Environment.TickCount64 > deadline) throw new Exception("Lease operation stalled.");
            Thread.SpinWait(50); Thread.Yield();
        }
        return task.GetAwaiter().GetResult();
    }
    static void DirectAwait(byte[] file, McapCompression compression)
    {
        using var source = new SuspendingStream(file, 512);
        using var reader = new McapAsyncReader(source, leaveOpen: true, inputBufferSize: 512);
        Consume().GetAwaiter().GetResult();
        source.WaitIdle();
        if (source.Suspensions == 0) throw new Exception("Direct lease await did not suspend.");
        Console.WriteLine($"lease direct await/{compression}: {Warmup + Count} batches; continuations safely reenter on the I/O thread");
        async Task Consume()
        {
            for (int i = 0; i < Warmup + Count; i++)
            {
                using var batch = await reader.ReadBatchLeaseAsync(1).ConfigureAwait(false);
                if (batch is null || batch.Count != 1 || batch.GetHeader(0).Sequence != (uint)i)
                    throw new Exception("Direct-await lease order changed.");
            }
            if (await reader.ReadBatchLeaseAsync(1).ConfigureAwait(false) is not null)
                throw new Exception("Expected EOF after direct-await leases.");
        }
    }
}
