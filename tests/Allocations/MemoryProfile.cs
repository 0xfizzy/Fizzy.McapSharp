using System.Diagnostics;
using System.Text.Json;
using System.Runtime.CompilerServices;
using Fizzy.McapSharp;

// Diagnostic process samples, deliberately separate from exact allocation gates.
static class MemoryProfile
{
    public static void Run(int count)
    {
        if (count < 8192) throw new ArgumentOutOfRangeException(nameof(count));
        foreach (var compression in Enum.GetValues<McapCompression>())
        foreach (bool seekable in new[] { true, false })
        foreach (string indexes in new[] { "all", "no-message", "no-chunk", "no-attachment", "no-metadata" })
        foreach (var setting in new[] { (Chunk: 1UL << 20, FlushEvery: 0), (Chunk: 4UL << 20, FlushEvery: 0),
            (Chunk: 16UL << 20, FlushEvery: 0), (Chunk: 1UL << 20, FlushEvery: 64) })
        {
            using var stream = new CountingStream(seekable);
            using var writer = new McapWriter(stream, new() { Compression = compression, ChunkSize = setting.Chunk,
                EmitMessageIndexes = indexes != "no-message", EmitChunkIndexes = indexes != "no-chunk",
                EmitAttachmentIndexes = indexes != "no-attachment", EmitMetadataIndexes = indexes != "no-metadata" });
            var channel = writer.RegisterChannel("t", "raw");
            var data = new byte[1024]; Array.Fill(data, (byte)42);
            var watch = Stopwatch.StartNew();
            Sample(0, "writing");
            for (uint i = 0; i < count; i++)
            {
                writer.WriteMessage(new(channel, i, i, 0), data);
                if (setting.FlushEvery != 0 && (i + 1) % setting.FlushEvery == 0) writer.Flush();
                if ((i + 1) % 8192 == 0) Sample(i + 1, "writing");
            }
            // Attachments finish chunks; keep them after the message phase so they
            // do not override the 16 MiB chunk target under measurement.
            for (uint i = 0; i < count / 256; i++)
            {
                writer.WriteAttachment("attachment", "raw", i, i, data);
                writer.WriteMetadata("metadata", new Dictionary<string, string> { ["key"] = "value" });
                if ((i + 1) % 32 == 0) Sample((uint)count, "control-records", i + 1);
            }
            writer.Complete(); Sample((uint)count, "completed");
            var (chunkCount, summaryBytes) = ObserveSummary(writer, () => Sample((uint)count, "summary-held"));
            GC.Collect(); GC.WaitForPendingFinalizers(); GC.Collect();
            Sample((uint)count, "summary-released");
            writer.Dispose(); Sample((uint)count, "disposed");
            Console.WriteLine(JsonSerializer.Serialize(new { kind = "writer-summary-size", compression = compression.ToString(),
                seekable, indexes, chunk = setting.Chunk, setting.FlushEvery, chunkCount, summaryManagedAllocatedBytes = summaryBytes }));
            void Sample(uint messages, string state, uint controlPairs = 0)
            {
                using var process = Process.GetCurrentProcess(); process.Refresh();
                Console.WriteLine(JsonSerializer.Serialize(new { compression = compression.ToString(), seekable, indexes,
                    chunk = setting.Chunk, setting.FlushEvery, messages, controlPairs, state, elapsedMs = watch.ElapsedMilliseconds, privateBytes = process.PrivateMemorySize64,
                    workingSetBytes = process.WorkingSet64, managedHeapBytes = GC.GetTotalMemory(false), outputBytes = stream.Length }));
            }
        }
    }
    // Keep the result rooted through sampling, then leave the frame before collecting.
    [MethodImpl(MethodImplOptions.NoInlining)]
    static (int Chunks, long Allocated) ObserveSummary(McapWriter writer, Action sample)
    {
        long before = GC.GetAllocatedBytesForCurrentThread();
        var summary = writer.GetSummary();
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
        sample();
        int chunks = summary.ChunkIndexes.Count;
        GC.KeepAlive(summary);
        return (chunks, allocated);
    }
    sealed class CountingStream(bool seekable) : Stream
    {
        long position, length;
        public override bool CanRead => false;
        public override bool CanWrite => true;
        public override bool CanSeek => seekable;
        public override long Length => length;
        public override long Position { get => position; set => Seek(value, SeekOrigin.Begin); }
        public override void Write(ReadOnlySpan<byte> buffer) { position += buffer.Length; length = Math.Max(length, position); }
        public override void Write(byte[] buffer, int offset, int count) => throw new InvalidOperationException("Array fallback");
        public override long Seek(long offset, SeekOrigin origin)
        {
            if (!seekable) throw new NotSupportedException();
            return position = origin switch { SeekOrigin.Begin => offset, SeekOrigin.Current => position + offset, _ => length + offset };
        }
        public override void Flush() { }
        public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
    }
}
