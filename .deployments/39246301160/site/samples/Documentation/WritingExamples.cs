using Fizzy.McapSharp;

internal static class WritingExamples
{
    internal static void CreateRecording(string path, bool includeExtras = true)
    {
        using var writer = new McapWriter(path);
        var schema = writer.RegisterSchema("value", "jsonschema", "{\"type\":\"object\"}"u8);
        var values = writer.RegisterChannel("/values", "json", schema);
        var events = writer.RegisterChannel("/events", "text/plain");
        writer.WriteToKnownChannel(new(values, 0, 100, 90), "{\"value\":1}"u8);
        writer.WriteToKnownChannel(new(events, 0, 50, 50), "started"u8);
        writer.WriteToKnownChannel(new(events, 1, 110, 110), []);
        if (includeExtras) AddMetadataAndAttachments(writer);
        writer.Complete();
        writer.FlushToDisk();
    }

    private static void AddMetadataAndAttachments(McapWriter writer)
    {
        writer.WriteMetadata("recording", new Dictionary<string, string> { ["operator"] = "sample" });
        writer.WriteAttachment("notes.txt", "text/plain", 100, 90, "notes"u8);
        writer.StartAttachment("parts.txt", "text/plain", 100, 90, 6);
        writer.WriteAttachmentBytes("abc"u8);
        writer.WriteAttachmentBytes("def"u8);
        writer.FinishAttachment();
    }

    internal static void AutomaticDeclarations()
    {
        using var output = new MemoryStream();
        var schema = new McapSchema(1, "value", "jsonschema", "{}"u8.ToArray());
        var channel = new McapChannel(7, "/values", "json", schema, new Dictionary<string, string>());
        using (var writer = new McapWriter(output, leaveOpen: true))
        {
            writer.WriteMessage(new McapMessage(channel, 0, 100, 90, "{}"u8.ToArray()));
            using var prepared = new McapPreparedChannel(channel);
            writer.WriteMessage(prepared, new(channel.Id, 1, 200, 190), "{}"u8);
            writer.Complete();
        }
        output.Position = 0;
        using var reader = McapReaderFactory.OpenMessages(output, leaveOpen: true);
        var messages = reader.ReadMessages().ToArray();
        if (messages.Length != 2 || messages[1].Sequence != 1 ||
            messages.Any(m => m.Channel.Id != 7 || m.Channel.Schema?.Name != "value" || !m.Data.AsSpan().SequenceEqual("{}"u8)))
            throw new Exception("Automatic declarations or prepared write mismatch.");
    }

    internal static void SafeRejections()
    {
        foreach (var strict in new[] { false, true })
        {
            using var output = new MemoryStream();
            var options = strict ? new McapWriterOptions { SafeRejections = McapWriterSafeRejections.None } : new();
            using var writer = new McapWriter(output, options, leaveOpen: true);
            McapException? rejection = null;
            try { writer.WriteToKnownChannel(new(9, 0, 0, 0), []); }
            catch (McapException error) { rejection = error; }
            if (rejection is null || rejection.CanContinueWriting == strict)
                throw new Exception("Unexpected safe rejection policy.");
            if (strict)
            {
                try { writer.Complete(); }
                catch (InvalidOperationException) { continue; }
                throw new Exception("Strict writer should be terminal.");
            }
            writer.RegisterChannel(9, "/events", "text/plain");
            writer.WriteToKnownChannel(new(9, 0, 0, 0), "ok"u8);
            writer.Complete();
            writer.Dispose(); // Release ownership before the caller repositions the Stream.
            output.Position = 0;
            using var reader = McapReaderFactory.OpenMessages(output, leaveOpen: true);
            if (!reader.ReadMessages().Single().Data.AsSpan().SequenceEqual("ok"u8))
                throw new Exception("Writing after an audited rejection failed.");
        }
    }

    internal static void Configurations()
    {
        foreach (var compression in new[] { McapCompression.None, McapCompression.Lz4, McapCompression.Zstd })
        {
            using var output = new MemoryStream();
            using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 1024 }, leaveOpen: true))
            {
                var channel = writer.RegisterChannel("/sample", "raw");
                writer.WriteToKnownChannel(new(channel, 0, 100, 100), new byte[2048]);
                writer.WriteToKnownChannel(new(channel, 1, 50, 50), [42]);
                writer.Complete();
            }
            output.Position = 0;
            using var reader = McapReaderFactory.QueryMessages(output,
                new() { Order = McapReadOrder.LogTime, AllowBufferedSort = false }, leaveOpen: true);
            var messages = reader.ReadMessages().ToArray();
            if (messages.Length != 2 || messages[0].LogTime != 50 || !messages[0].Data.AsSpan().SequenceEqual(new byte[] { 42 }) ||
                messages[1].LogTime != 100 || !messages[1].Data.AsSpan().SequenceEqual(new byte[2048]))
                throw new Exception($"Compression round trip failed: {compression}.");
        }
        using var unindexed = new MemoryStream();
        using (var writer = new McapWriter(unindexed, new() { UseChunks = false, EmitSummaryRecords = false }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("/sample", "raw");
            writer.WriteToKnownChannel(new(channel, 0, 100, 100), [42]);
            writer.Complete();
        }
        unindexed.Position = 0;
        try
        {
            using var query = McapReaderFactory.QueryMessages(unindexed,
                new() { Order = McapReadOrder.LogTime, AllowBufferedSort = false }, leaveOpen: true);
        }
        catch (NotSupportedException) { return; }
        throw new Exception("Unindexed sorting fallback should be rejected.");
    }

    internal static void OutputLifecycle(string directory)
    {
        var path = Path.Combine(directory, "stream.mcap");
        using (var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write))
        {
            using (var writer = new McapWriter(output, leaveOpen: true))
            {
                var channel = writer.RegisterChannel("/sample", "raw");
                writer.WriteToKnownChannel(new(channel, 0, 0, 0), []);
                writer.Complete();
                writer.FlushToDisk();
            }
            if (!output.CanWrite) throw new Exception("leaveOpen did not retain output.");
        }
        var original = File.ReadAllBytes(path);
        bool rejected = false;
        try { using var writer = new McapWriter(path); }
        catch (McapException) { rejected = true; }
        if (!rejected || !File.ReadAllBytes(path).AsSpan().SequenceEqual(original))
            throw new Exception("Existing path must be rejected without modification.");
        if (new McapReaderFactory(path).Validate() == 0) throw new Exception("Stream recording is empty.");
    }
}
