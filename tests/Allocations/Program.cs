using Fizzy.McapSharp;
using System.Diagnostics;

#if DEBUG
throw new InvalidOperationException("Run allocation acceptance in Release.");
#endif
if (args.Length > 0 && args[0] == "memory-profile") { MemoryProfile.Run(args.Length > 1 ? int.Parse(args[1]) : 65536); return; }
if (args.Contains("lease-profile")) { LeaseGate.Run(false); return; }
if (args.Contains("lease-gate")) { LeaseGate.Run(true); return; }
if (args.Contains("extended")) { Extended.Run(); MemoryGate.Run(); ConvenienceGate.Run(); return; }
BatchGate.Run();
const int count = 10000;
foreach (var compression in Enum.GetValues<McapCompression>())
    foreach (var mode in new[]
    {
        "file",
        "filestream",
        "seekable",
        "nonseekable"
    }

    )
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        var payload = new byte[1024];
        using var storage = new MemoryStream(32 * 1024 * 1024);
        using var fileStream = mode == "filestream" ? new FileStream(path, FileMode.CreateNew, FileAccess.ReadWrite, FileShare.None, 65536, FileOptions.None) : null;
        using (var w = mode == "file" ? new McapWriter(path, new() { Compression = compression, ChunkSize = 16384 }) : new McapWriter(fileStream is not null ? fileStream : new SpanStream(storage, mode == "seekable"), new() { Compression = compression, ChunkSize = 16384 }, true))
        {
            try { w.WriteMessage(new McapMessageHeader(99, 0, 0, 0), payload); }
            catch (McapException e) when (e.CanContinueWriting) { }
            var c = w.RegisterChannel("t", "raw");
            for (uint i = 0; i < 1000; i++)
                w.WriteMessage(new(c, i, i, i), payload);
            var bytes = GC.GetAllocatedBytesForCurrentThread();
            var start = Stopwatch.GetTimestamp();
            for (uint i = 0; i < count; i++)
                w.WriteMessage(new(c, i, i, i), payload);
            var allocated = GC.GetAllocatedBytesForCurrentThread() - bytes;
            var elapsed = Stopwatch.GetElapsedTime(start);
            Report("write", allocated, count, elapsed);
            var c2 = w.RegisterChannel("new", "raw");
            w.WriteMessage(new(c2, 0, 0, 0), []);
            w.WriteMessage(new(c2, 1, 1, 1), new byte[100000]);
            w.Complete();
        }

        storage.Position = 0;
        if (fileStream is not null)
            fileStream.Position = 0;
        using (var r = mode == "file" ? new McapFileReader(path).OpenMessages() : McapFileReader.OpenMessages(fileStream is not null ? fileStream : new SpanStream(storage, mode == "seekable"), leaveOpen: true))
        {
            var buffer = new byte[100000];
            for (int i = 0; i < 1000; i++)
                r.ReadNext(buffer, out _, out _);
            var bytes = GC.GetAllocatedBytesForCurrentThread();
            var start = Stopwatch.GetTimestamp();
            int n = 0;
            while (true)
            {
                var status = r.ReadNext([], out _, out _);
                if (status == McapReadStatus.EndOfStream)
                    break;
                if (status == McapReadStatus.BufferTooSmall && r.ReadNext(buffer, out _, out _) != McapReadStatus.Success)
                    throw new Exception("Retry failed");
                n++;
            }

            for (int i = 0; i < 1000; i++)
                r.ReadNext([], out _, out _);
            var allocated = GC.GetAllocatedBytesForCurrentThread() - bytes;
            var elapsed = Stopwatch.GetElapsedTime(start);
            Report("read", allocated, n, elapsed);
            if (n != count + 2)
                throw new Exception("Unexpected message count");
        }

        storage.Position = 0;
        if (fileStream is not null)
            fileStream.Position = 0;
        using (var r = mode == "file" ? new McapFileReader(path).OpenMessages(new() { Topic = "t" }) : McapFileReader.OpenMessages(fileStream is not null ? fileStream : new SpanStream(storage, mode == "seekable"), new() { Topic = "t" }, true))
        {
            var buffer = new byte[1024];
            for (int i = 0; i < 1000; i++)
                r.ReadNext(buffer, out _, out _);
            var bytes = GC.GetAllocatedBytesForCurrentThread();
            var start = Stopwatch.GetTimestamp();
            int n = 0;
            while (r.ReadNext(buffer, out _, out _) != McapReadStatus.EndOfStream)
                n++;
            var allocated = GC.GetAllocatedBytesForCurrentThread() - bytes;
            var elapsed = Stopwatch.GetElapsedTime(start);
            Report("query", allocated, n, elapsed);
            if (n != count)
                throw new Exception("Unexpected query count");
        }

        fileStream?.Dispose();
        if (mode is "file" or "filestream")
            File.Delete(path);
        void Report(string operation, long bytes, int n, TimeSpan elapsed)
        {
            Console.WriteLine($"{compression}/{mode} {operation}: {bytes} B total, {elapsed.TotalNanoseconds / n:F1} ns/message, {n / elapsed.TotalSeconds:F0} msg/s");
            if (bytes != 0)
                throw new Exception($"Allocation regression: {bytes} bytes");
        }
    }

Extended.Run();
MemoryGate.Run(); ConvenienceGate.Run();
LeaseGate.Run(true);

sealed class SpanStream(Stream inner, bool seekable) : Stream
{
    public override bool CanRead => true;
    public override bool CanWrite => true;
    public override bool CanSeek => seekable;
    public override long Length => inner.Length;
    public override long Position { get => inner.Position; set => inner.Position = value; }

    public override int Read(Span<byte> b) => inner.Read(b);
    public override void Write(ReadOnlySpan<byte> b) => inner.Write(b);
    public override int Read(byte[] b, int o, int n) => throw new Exception("Array fallback");
    public override void Write(byte[] b, int o, int n) => throw new Exception("Array fallback");
    public override long Seek(long o, SeekOrigin origin) => seekable ? inner.Seek(o, origin) : throw new NotSupportedException();
    public override void Flush() => inner.Flush();
    public override void SetLength(long n) => inner.SetLength(n);
}
