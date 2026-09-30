using System.Diagnostics;
using System.Text.Json;
using Fizzy.McapSharp;

// Diagnostic process samples, deliberately separate from exact allocation gates.
static class MemoryProfile
{
    public static void Run(int count)
    {
        if (count < 8192) throw new ArgumentOutOfRangeException(nameof(count));
        foreach (var compression in Enum.GetValues<McapCompression>())
        foreach (bool seekable in new[] { true, false })
        foreach (bool indexes in new[] { true, false })
        foreach (ulong? chunk in new ulong?[] { 65536, null })
        {
            using var stream = new CountingStream(seekable);
            using var writer = new McapWriter(stream, new() { Compression = compression, ChunkSize = chunk,
                EmitMessageIndexes = indexes, EmitChunkIndexes = indexes });
            var channel = writer.RegisterChannel("t", "raw");
            var data = new byte[1024]; Array.Fill(data, (byte)42);
            var watch = Stopwatch.StartNew();
            Sample(0, "writing");
            for (uint i = 0; i < count; i++)
            {
                writer.WriteMessage(new(channel, i, i, 0), data);
                if ((i + 1) % 8192 == 0) Sample(i + 1, "writing");
            }
            writer.Complete(); Sample((uint)count, "completed");
            writer.Dispose(); Sample((uint)count, "disposed");
            void Sample(uint messages, string state)
            {
                using var process = Process.GetCurrentProcess(); process.Refresh();
                Console.WriteLine(JsonSerializer.Serialize(new { compression = compression.ToString(), seekable, indexes,
                    chunk, messages, state, elapsedMs = watch.ElapsedMilliseconds, privateBytes = process.PrivateMemorySize64,
                    workingSetBytes = process.WorkingSet64, managedHeapBytes = GC.GetTotalMemory(false), outputBytes = stream.Length }));
            }
        }
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
