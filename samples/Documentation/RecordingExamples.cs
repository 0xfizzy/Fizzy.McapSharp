using Fizzy.McapSharp;
using System.Text;

internal static class RecordingExamples
{
    internal static void Run()
    {
        var directory = Path.Combine(Path.GetTempPath(), $"mcap-guides-{Guid.NewGuid():N}");
        Directory.CreateDirectory(directory);
        try
        {
            var basicPath = Path.Combine(directory, "messages.mcap");
            WritingExamples.CreateRecording(basicPath, includeExtras: false);
            var basic = new McapReaderFactory(basicPath);
            if (!basic.ReadMessages().Select(m => m.LogTime).SequenceEqual(new ulong[] { 100, 50, 110 }) ||
                basic.ReadMetadata().Any() || basic.ReadAttachments().Any())
                throw new Exception("Basic recording differs from the guide.");
            var path = Path.Combine(directory, "recording.mcap");
            WritingExamples.CreateRecording(path);
            ReadRecording(path);
            Forward(path);
            WritingExamples.AutomaticDeclarations();
            WritingExamples.SafeRejections();
            WritingExamples.Configurations();
            WritingExamples.OutputLifecycle(directory);
            ValidationExamples.Run(directory);
        }
        finally { Directory.Delete(directory, recursive: true); }
    }

    internal static void ReadRecording(string path)
    {
        var factory = new McapReaderFactory(path);
        foreach (var message in factory.ReadMessages())
            Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
        var filtered = factory.ReadMessages(new()
        {
            Topic = "/events",
            StartTime = 50,
            EndTime = 110
        }).ToArray();
        foreach (var message in filtered)
            Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
        if (filtered.Length != 1 || filtered[0].LogTime != 50 ||
            Encoding.UTF8.GetString(filtered[0].Data) != "started")
            throw new Exception("Guide filter output differs.");
        foreach (var metadata in factory.ReadMetadata())
            Console.WriteLine($"{metadata.Name}: {metadata.Values["operator"]}");
        foreach (var attachment in factory.ReadAttachments())
            Console.WriteLine($"{attachment.Name}: {Encoding.UTF8.GetString(attachment.Data)}");
        var summary = factory.GetSummary();
        Console.WriteLine(summary is null ? "No summary" : "Summary available");
        var messages = factory.ReadMessages().ToArray();
        if (!messages.Select(m => m.LogTime).SequenceEqual(new ulong[] { 100, 50, 110 }) ||
            messages[0].Channel.Schema?.Name != "value" || messages[1].Channel.Schema is not null ||
            messages[0].Channel.Topic != "/values" || messages[1].Channel.Topic != "/events" ||
            !messages[0].Data.AsSpan().SequenceEqual("{\"value\":1}"u8) ||
            !messages[1].Data.AsSpan().SequenceEqual("started"u8) || messages[2].Data.Length != 0)
            throw new Exception("Recording messages or declarations differ.");
        if (factory.ReadSchemas().Count() != 1 || factory.ReadChannels().Count() != 2 ||
            factory.ReadMetadata().Single().Values["operator"] != "sample")
            throw new Exception("Recording declarations or metadata differ.");
        var attachments = factory.ReadAttachments().ToDictionary(a => a.Name);
        if (attachments.Count != 2 || !attachments["notes.txt"].Data.AsSpan().SequenceEqual("notes"u8) ||
            !attachments["parts.txt"].Data.AsSpan().SequenceEqual("abcdef"u8))
            throw new Exception("Recording attachments differ.");
        if (factory.GetSummary() is null || factory.Validate() <= 3)
            throw new Exception("Recording summary or validation missing.");

        using var reader = factory.OpenMessages();
        byte[] buffer = [];
        int count = 0, retries = 0;
        while (true)
        {
            var status = reader.ReadNext(buffer, out var header, out var length);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)length)];
                retries++;
                continue;
            }
            if (header.LogTime != messages[count].LogTime ||
                !buffer.AsSpan(0, checked((int)length)).SequenceEqual(messages[count].Data))
                throw new Exception("Pending message changed during retry.");
            count++;
        }
        if (count != 3 || retries == 0 || reader.ReadNext(buffer, out _, out _) != McapReadStatus.EndOfStream)
            throw new Exception("Buffer retry, empty message or EOF mismatch.");
    }

    internal static void Forward(string path)
    {
        var factory = new McapReaderFactory(path);
        var declarations = factory.ReadChannels().ToArray();
        McapMessageBatchLease batch;
        using (var reader = factory.OpenMessages()) batch = reader.ReadBatchLease(10)!;
        using (batch)
        using (var output = new MemoryStream())
        {
            using (var writer = new McapWriter(output, leaveOpen: true))
            {
                var ids = new Dictionary<ushort, ushort>();
                foreach (var channel in declarations)
                {
                    ushort schemaId = channel.Schema is { } schema
                        ? writer.RegisterSchema(schema.Name, schema.Encoding, schema.Data) : (ushort)0;
                    ids[channel.Id] = writer.RegisterChannel(checked((ushort)(channel.Id + 100)),
                        channel.Topic, channel.MessageEncoding, schemaId, channel.Metadata);
                }
                var headers = new McapMessageHeader[batch.Count];
                for (int i = 0; i < batch.Count; i++)
                {
                    var header = batch.GetHeader(i);
                    headers[i] = header with { ChannelId = ids[header.ChannelId] };
                }
                if (writer.WriteBatch(batch, headers) != batch.Count) throw new Exception("Forward count mismatch.");
                writer.Complete();
            }
            output.Position = 0;
            using var forwarded = McapReaderFactory.OpenMessages(output, leaveOpen: true);
            var copies = forwarded.ReadMessages().ToArray();
            if (copies.Length != batch.Count || copies.Length != 3) throw new Exception("Forwarded messages missing.");
            for (int i = 0; i < copies.Length; i++)
                if (copies[i].Channel.Id != batch.GetHeader(i).ChannelId + 100 ||
                    copies[i].LogTime != batch.GetHeader(i).LogTime ||
                    !copies[i].Data.AsSpan().SequenceEqual(batch.GetPayload(i)))
                    throw new Exception("Forwarded header or payload mismatch.");
        }
    }
}
