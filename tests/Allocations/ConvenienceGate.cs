using Fizzy.McapSharp;

static class ConvenienceGate
{
    public static void Run()
    {
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
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

    static long Measure(byte[] bytes, int kind)
    {
        using var stream = new MemoryStream(bytes);
        using var session = McapReader.OpenRecords(stream);
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
