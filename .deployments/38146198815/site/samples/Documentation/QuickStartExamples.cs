using Fizzy.McapSharp;
using System.Text;

internal static class QuickStartExamples
{
    internal static void Run()
    {
        var path = Path.Combine(Path.GetTempPath(), $"sample-{Guid.NewGuid():N}.mcap");
        try
        {
            using (var writer = new McapWriter(path))
            {
                var channel = writer.RegisterChannel("/sample", "json");
                writer.WriteToKnownChannel(new McapMessageHeader(channel, 0, 100, 100), "{\"value\":1}"u8);
                writer.WriteToKnownChannel(new McapMessageHeader(channel, 1, 50, 50), "{\"value\":2}"u8);
                writer.Complete();
            }
            var reader = new McapReaderFactory(path);
            var lines = reader.ReadMessages()
                .Select(message => $"{message.LogTime}: {Encoding.UTF8.GetString(message.Data)}").ToArray();
            if (!lines.SequenceEqual(new[] { "100: {\"value\":1}", "50: {\"value\":2}" }))
                throw new Exception("Quick-start output or file order mismatch.");
            foreach (var line in lines) Console.WriteLine(line);
            var selected = reader.ReadMessages(new() { Topic = "/sample", StartTime = 50, EndTime = 100 }).ToArray();
            if (selected.Length != 1 || selected[0].LogTime != 50 ||
                Encoding.UTF8.GetString(selected[0].Data) != "{\"value\":2}")
                throw new Exception("Quick-start inclusive/exclusive time filter mismatch.");
            if (reader.ReadMessages(new() { Topic = "/missing" }).Any())
                throw new Exception("Quick-start topic filter mismatch.");
            ulong recordCount = reader.Validate();
            if (recordCount <= (ulong)lines.Length)
                throw new Exception("Validation should include records beyond messages.");
            Console.WriteLine($"Validated {recordCount} records.");
        }
        finally { File.Delete(path); }
    }
}
