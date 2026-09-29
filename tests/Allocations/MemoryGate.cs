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
            }
            finally { File.Delete(path); }
        }
    }
}
