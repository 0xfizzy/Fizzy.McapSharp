using Fizzy.McapSharp;

static class MemoryGate
{
    public static void Run()
    {
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
            string path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
            try
            {
                byte[] payload = new byte[256];
                using (var writer = new McapWriter(path, new() { Compression = compression, ChunkSize = null }))
                {
                    var channel = writer.RegisterChannel("t", "raw");
                    for (uint i = 0; i < 2000; i++) writer.WriteMessage(new(channel, i, i, 0), payload);
                    writer.Complete();
                }
                using var snapshot = McapIndexSnapshot.OpenMapped(path, new() { MaxPendingBufferBytes = 0 });
                using var reader = snapshot.OpenChunkReader(snapshot.GetSummary()!.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0));
                for (int i = 0; i < 100; i++) { reader.ReadNext(payload, out _, out _); _ = reader.GetMemoryStatistics(); _ = snapshot.GetMemoryStatistics(); }
                long before = GC.GetAllocatedBytesForCurrentThread();
                while (reader.ReadNext(payload, out _, out _) != McapReadStatus.EndOfStream)
                { _ = reader.GetMemoryStatistics(); _ = snapshot.GetMemoryStatistics(); }
                reader.ReadNext(payload, out _, out _);
                long bytes = GC.GetAllocatedBytesForCurrentThread() - before;
                if (bytes != 0) throw new Exception($"Mapped cursor/statistics allocated {bytes} B");
                Console.WriteLine($"mapped cursor/statistics {compression}: {bytes} B");
                using var mapped = McapBufferReader.OpenMapped(path);
                for (int i = 0; i < 100; i++) mapped.ReadNext(payload, out _, out _);
                before = GC.GetAllocatedBytesForCurrentThread();
                while (mapped.ReadNext(payload, out _, out _) != McapReadStatus.EndOfStream) _ = mapped.GetMemoryStatistics();
                bytes = GC.GetAllocatedBytesForCurrentThread() - before;
                if (bytes != 0) throw new Exception($"Mapped reader allocated {bytes} B");
                using var cached = McapIndexSnapshot.OpenMapped(path, new() { MaxRandomAccessCacheBytes = 1024 * 1024 });
                var chunk = cached.GetSummary()!.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0);
                var entry = cached.ReadMessageIndexes(chunk)[0].Records.Last();
                using var input = File.OpenRead(path);
                using var session = McapReader.OpenMessages(input);
                var scratch = new byte[1024];
                for (int i = 0; i < 100; i++) { cached.SeekMessage(chunk, entry, payload, out _, out _); session.ReadRecordAt(8, scratch, out _, out _); }
                before = GC.GetAllocatedBytesForCurrentThread();
                for (int i = 0; i < 1000; i++)
                {
                    cached.SeekMessage(chunk, entry, payload, out _, out _);
                    session.ReadRecordAt(8, scratch, out _, out _);
                    _ = cached.GetMemoryStatistics(); _ = session.GetMemoryStatistics();
                }
                bytes = GC.GetAllocatedBytesForCurrentThread() - before;
                if (bytes != 0) throw new Exception($"Cached seeks/scratch allocated {bytes} B");
                Console.WriteLine($"mapped reader/cached seek/scratch {compression}: 0 B");
            }
            finally { File.Delete(path); }
        }
    }
}
