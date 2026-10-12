using Fizzy.McapSharp;

internal static class ReadingWorkflowExamples
{
    internal static async Task RunAsync()
    {
        var path = Path.Combine(Path.GetTempPath(), $"mcap-docs-{Guid.NewGuid():N}.mcap");
        try
        {
            using (var writer = new McapWriter(path))
            {
                var channel = writer.RegisterChannel("/sample", "json");
                writer.WriteToKnownChannel(new(channel, 0, 100, 100), "{\"value\":1}"u8);
                writer.WriteToKnownChannel(new(channel, 1, 50, 50), "{\"value\":2}"u8);
                writer.Complete();
            }
            ReadingExamples.Queries(path);
            ReadingExamples.Ownership(path);
            ReadingExamples.Streams(path);
            await ReadingExamples.Async(path);
            ForwardWithMatchingIds(path);
            SansIoExamples.Run(path);
        }
        finally { File.Delete(path); }
    }

    static void ForwardWithMatchingIds(string path)
    {
        McapMessageBatchLease batch;
        using (var reader = new McapReaderFactory(path).OpenMessages()) batch = reader.ReadBatchLease()!;
        using (batch)
        using (var output = new MemoryStream())
        {
            using (var writer = new McapWriter(output, leaveOpen: true))
            {
                writer.RegisterChannel(batch.GetHeader(0).ChannelId, "/sample", "json");
                if (writer.WriteBatch(batch) != 2) throw new Exception("Forward count mismatch.");
                writer.Complete();
            }
            output.Position = 0;
            using var reader = McapReaderFactory.OpenMessages(output, leaveOpen: true);
            var messages = reader.ReadMessages().ToArray();
            if (messages.Length != 2) throw new Exception("Forwarded messages missing.");
            for (int i = 0; i < messages.Length; i++)
                if (!messages[i].Data.AsSpan().SequenceEqual(batch.GetPayload(i)))
                    throw new Exception("Forwarded payload differs.");
        }
    }
}
