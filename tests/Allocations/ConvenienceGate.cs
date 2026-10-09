using Fizzy.McapSharp;

static class ConvenienceGate
{
    public static void Run()
    {
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
            CheckOwnedPayloadBudget(compression);
            CheckPreparedIndex(compression);
            var small = Recording(32, compression);
            var large = Recording(4096, compression);
            for (int kind = 0; kind < 4; kind++)
            {
                Measure(small, kind); Measure(large, kind);
                long baseline = Measure(small, kind), expanded = Measure(large, kind);
                // Startup/target models may allocate; unrelated fixed-size messages must not.
                if (expanded > baseline + 4096)
                    throw new Exception($"Classification {kind}/{compression}: allocation grew {baseline} -> {expanded} B");
                Console.WriteLine($"classification {kind}/{compression}: {baseline} -> {expanded} B");
            }
        }
    }

    static void CheckOwnedPayloadBudget(McapCompression compression)
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { Compression = compression }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            byte[] payload = new byte[100000];
            for (uint i = 0; i < 64; i++) writer.WriteMessage(new(channel, i, i, 0), payload);
            writer.Complete();
        }
        var bytes = storage.ToArray();
        foreach (bool buffer in new[] { false, true })
        {
            using var session = McapReaderFactory.OpenMessages(new MemoryStream(bytes));
            using var reader = new McapReadCursor(bytes);
            using var messages = (buffer ? reader.ReadMessages() : session.ReadMessages()).GetEnumerator();
            for (int i = 0; i < 4; i++) messages.MoveNext();
            long before = GC.GetAllocatedBytesForCurrentThread();
            int count = 0;
            while (messages.MoveNext()) { if (messages.Current.Data.Length != 100000) throw new Exception("Payload length"); count++; }
            long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
            if (count != 60 || allocated > count * 101000L) throw new Exception($"Owned payload allocation {allocated} B for {count} messages");
            Console.WriteLine($"owned payload/{buffer}/{compression}: {allocated} B ({count} final arrays)");
        }
    }
    static void CheckPreparedIndex(McapCompression compression)
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { Compression = compression, ChunkSize = null }, true))
        {
            for (uint i = 0; i < 110; i++)
            {
                var channel = writer.RegisterChannel("t" + i, "raw");
                writer.WriteMessage(new(channel, i, i, 0), new byte[64]);
            }
            writer.Complete();
        }
        using var snapshot = new McapIndexSnapshot(storage.ToArray(), new() { MaxRandomAccessCacheBytes = 1024 * 1024 });
        using var index = new McapPreparedChunkIndex(snapshot.GetSummary()!.ChunkIndexes.Single());
        var entry = snapshot.ReadMessageIndexes(index)[0].Records[0];
        byte[] output = new byte[64];
        for (int i = 0; i < 100; i++) { snapshot.SeekMessage(index, entry, output, out _, out _); snapshot.GetCompressedDataOffset(index); }
        long before = GC.GetAllocatedBytesForCurrentThread();
        for (int i = 0; i < 1000; i++) { snapshot.SeekMessage(index, entry, output, out _, out _); snapshot.GetCompressedDataOffset(index); }
        long bytes = GC.GetAllocatedBytesForCurrentThread() - before;
        if (bytes != 0) throw new Exception($"Prepared index allocated {bytes} B");
        Console.WriteLine($"prepared large index/{compression}: 0 B");
    }

    static long Measure(byte[] bytes, int kind)
    {
        using var stream = new MemoryStream(bytes);
        using var session = McapReaderFactory.OpenRecords(stream);
        long before = GC.GetAllocatedBytesForCurrentThread();
        int count = kind switch
        {
            0 => session.ReadSchemas().Count(),
            1 => session.ReadChannels().Count(),
            2 => session.ReadMetadata().Count(),
            _ => session.ReadAttachments().Count()
        };
        long allocated = GC.GetAllocatedBytesForCurrentThread() - before;
        if (count != 1) throw new Exception("Missing classified record");
        return allocated;
    }

    static byte[] Recording(int count, McapCompression compression)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new()
        {
            Compression = compression, ChunkSize = 4096, EmitSummaryRecords = false,
            EmitSummaryOffsets = false, EmitMessageIndexes = false
        }, true))
        {
            var schema = writer.RegisterSchema("s", "raw", [1]);
            var channel = writer.RegisterChannel("t", "raw", schema);
            byte[] payload = new byte[1024];
            for (uint i = 0; i < count; i++) writer.WriteMessage(new(channel, i, i, i), payload);
            writer.WriteMetadata("m", new Dictionary<string, string> { ["k"] = "v" });
            writer.WriteAttachment("a", "raw", 0, 0, [1]);
            writer.Complete();
        }
        return stream.ToArray();
    }
}
