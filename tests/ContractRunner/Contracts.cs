using Fizzy.McapSharp;
using System.Diagnostics;
using System.Text.Json;

static class Contracts
{
    public static void Exchange(string directory)
    {
        var paths = Directory.GetFiles(directory, "*.mcap", SearchOption.AllDirectories);
        if (paths.Length != 18) throw new Exception("Expected all 18 platform/producer/compression fixtures");
        foreach (var path in paths)
        {
            var r = new McapReader(path); r.Validate();
            var messages = r.ReadMessages(new() { Topic = "/test", StartTime = 200, EndTime = 500 }).ToArray();
            if (!messages.Select(m => m.Sequence).SequenceEqual(new uint[] { 2, 3, 4 })) throw new Exception("Exchanged query mismatch");
            var origin = Path.GetFileName(path).StartsWith("python-") ? "python" : "dotnet";
            if (r.ReadMetadata().Single().Values["origin"] != origin || !r.ReadAttachments().Single().Data.SequenceEqual("attachment"u8.ToArray())) throw new Exception("Exchanged control records mismatch");
        }
    }
    public static async Task Check(string path, string spec)
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(spec));
        var expected = doc.RootElement.GetProperty("records").EnumerateArray().Where(r => r.GetProperty("type").GetString() == "Message").ToArray();
        void Compare(IEnumerable<McapMessage> messages, bool sorted, JsonElement[]? selection = null)
        {
            var actual = messages.Select(m => JsonSerializer.SerializeToElement(Canon.Message(m))).ToArray();
            ulong Time(JsonElement r) => ulong.Parse(Canon.Fields(r)["log_time"].GetString()!);
            uint Sequence(JsonElement r) => uint.Parse(Canon.Fields(r)["sequence"].GetString()!);
            var want = selection ?? expected;
            if (sorted)
            {
                if (!actual.Select(Time).SequenceEqual(actual.Select(Time).Order())) throw new Exception("LogTime order violated");
                // MCAP does not prescribe a cross-chunk tie order. Compare tied groups by unique sequence.
                actual = actual.OrderBy(Time).ThenBy(Sequence).ToArray();
                want = want.OrderBy(Time).ThenBy(Sequence).ToArray();
            }
            if (actual.Length != want.Length) throw new Exception("Message count mismatch");
            for (int i = 0; i < actual.Length; i++)
                if (JsonSerializer.Serialize(actual[i]) != JsonSerializer.Serialize(want[i])) throw new Exception($"Message {i} differs");
        }
        var reader = new McapReader(path);
        reader.Validate();
        Compare(reader.ReadMessages(new() { Order = McapReadOrder.File }), false);
        Compare(reader.ReadMessages(new() { Order = McapReadOrder.LogTime }), true);
        Compare(reader.ReadMessages(new() { Topic = "/测试", StartTime = 10, EndTime = 20 }), true,
            expected.Where(r => { var f = Canon.Fields(r); var t = ulong.Parse(f["log_time"].GetString()!); return f["channel_id"].GetString() == "1" && t >= 10 && t < 20; }).ToArray());
        using (var snapshot = new McapIndexSnapshot(File.ReadAllBytes(path)))
        {
            var summary = snapshot.GetSummary();
            if (summary is not null)
            {
                foreach (var index in summary.MetadataIndexes)
                    if (!reader.ReadMetadata().Any(m => m.Name == snapshot.ReadMetadata(index).Name)) throw new Exception("Indexed metadata mismatch");
                foreach (var index in summary.AttachmentIndexes)
                    if (!reader.ReadAttachments().Any(a => a.Data.SequenceEqual(snapshot.ReadAttachment(index).Data))) throw new Exception("Indexed attachment mismatch");
                foreach (var chunk in summary.ChunkIndexes)
                {
                    using var cursor = snapshot.OpenChunkReader(chunk);
                    var messages = cursor.ReadMessages().ToArray();
                    foreach (var index in snapshot.ReadMessageIndexes(chunk))
                        foreach (var entry in index.Records)
                        {
                            var actual = snapshot.SeekMessage(chunk, entry);
                            if (!messages.Any(m => m.Channel.Id == actual.Channel.Id && m.Sequence == actual.Sequence && m.Data.SequenceEqual(actual.Data))) throw new Exception("Random index mismatch");
                        }
                }
            }
        }
        using (var buffer = new McapBufferReader(File.ReadAllBytes(path))) Compare(buffer.ReadMessages(), false);
        using (var stream = File.OpenRead(path))
        using (var session = McapReader.OpenMessages(stream))
        {
            var data = new byte[8192]; var count = 0;
            while (true)
            {
                var status = session.ReadNext([], out var header, out var length);
                if (status == McapReadStatus.EndOfStream) break;
                if (length > (ulong)data.Length) data = new byte[checked((int)length)];
                if (status == McapReadStatus.BufferTooSmall)
                {
                    if (session.ReadNext(data, out var retried, out var copied) != McapReadStatus.Message || header != retried || length != copied) throw new Exception("Retry changed record");
                }
                var f = Canon.Fields(expected[count++]);
                if (header.Sequence != uint.Parse(f["sequence"].GetString()!) || !data.AsSpan(0, (int)length).SequenceEqual(f["data"].EnumerateArray().Select(x => byte.Parse(x.GetString()!)).ToArray())) throw new Exception("Buffer mismatch");
            }
            if (count != expected.Length) throw new Exception("Buffer count mismatch");
        }
        using (var stream = File.OpenRead(path))
        using (var asyncReader = new McapAsyncReader(stream, inputBufferSize: 3))
        {
            byte[] data = []; int count = 0;
            while (true)
            {
                var r = await asyncReader.ReadNextRecordAsync(data);
                if (r.Status == McapReadStatus.EndOfStream) break;
                if (r.Status == McapReadStatus.BufferTooSmall) { data = new byte[checked((int)r.Length)]; continue; }
                if (r.Opcode == 5)
                {
                    var actual = Canon.Record("Message", McapRecords.Parse(5, data.AsSpan(0, (int)r.Length)));
                    if (JsonSerializer.Serialize(actual) != JsonSerializer.Serialize(expected[count++])) throw new Exception("Async message mismatch");
                }
            }
            if (count != expected.Length) throw new Exception("Async count mismatch");
        }
        foreach (var type in new[] { "Schema", "Channel", "Attachment", "Metadata" })
        {
            var want = doc.RootElement.GetProperty("records").EnumerateArray().Where(r => r.GetProperty("type").GetString() == type).Select(r => JsonSerializer.Serialize(r)).Distinct().Order().ToArray();
            IEnumerable<object> actual = type switch {
                "Schema" => reader.ReadSchemas().Select(x => Canon.Record(type, x)),
                "Channel" => reader.ReadChannels().Select(x => Canon.Record(type, new McapChannelRecord(x.Id, x.Schema?.Id ?? 0, x.Topic, x.MessageEncoding, x.Metadata))),
                "Metadata" => reader.ReadMetadata().Select(x => Canon.Record(type, x)),
                _ => reader.ReadAttachments().Select(x => Canon.Record(type, new { x.LogTime, x.CreateTime, x.Name, x.MediaType, x.Data }))
            };
            var got = actual.Select(x => JsonSerializer.Serialize(x)).Distinct().Order().ToArray();
            if (!want.SequenceEqual(got)) throw new Exception(type + " mismatch");
        }
    }
    public static void Probe(string path)
    {
        // Only documented parser errors are acceptable. Unexpected CLR errors fail the process.
        try
        {
            using var stream = File.OpenRead(path);
            using var reader = McapReader.OpenRecords(stream, options: McapReaderOptions.Strict with { RecordLengthLimit = 8 * 1024 * 1024 });
            byte[] data = new byte[8 * 1024 * 1024];
            while (reader.ReadNextRecord(data, out _, out _) != McapReadStatus.EndOfStream) { }
        }
        catch (McapException e) { if (e.Message.Contains("panic", StringComparison.OrdinalIgnoreCase)) throw; }
    }
    public static async Task Lifecycle(string folder, int seconds)
    {
        var watch = Stopwatch.StartNew(); long iterations = 0;
        var samples = new List<(long Iterations, long PrivateBytes, int Handles)>();
        var path = Path.Combine(folder, "lifecycle.mcap");
        try
        {
            do
            {
                using (var w = new McapWriter(path)) { var c = w.RegisterChannel("lifecycle", "raw"); w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1]); w.Complete(); }
                var r = new McapReader(path);
                using (var s = r.OpenMessages()) using (var snapshot = s.OpenIndexSnapshot())
                    if (s.ReadMessages().Single().Data[0] != 1) throw new Exception("Lifecycle mismatch");
                using (var stream = File.OpenRead(path)) await using (var a = new McapAsyncReader(stream)) { await a.ReadNextRecordAsync(new byte[1024]); }
                File.Delete(path);
                if (++iterations % 100 == 0) { GC.Collect(); GC.WaitForPendingFinalizers(); using var p = Process.GetCurrentProcess(); samples.Add((iterations, p.PrivateMemorySize64, p.HandleCount)); }
            } while (watch.Elapsed.TotalSeconds < seconds);
        }
        finally { File.Delete(path); File.WriteAllText(Path.Combine(folder, "lifecycle.json"), JsonSerializer.Serialize(new { iterations, samples = samples.Select(s => new { s.Iterations, s.PrivateBytes, s.Handles }) })); }
        // Discard warmup; compare medians of two windows, never a single RSS sample.
        if (samples.Count >= 30)
        {
            var middle = samples.Skip(samples.Count / 3).Take(samples.Count / 3).ToArray();
            var tail = samples.Skip(2 * samples.Count / 3).ToArray();
            long Median(IEnumerable<long> values) { var a = values.Order().ToArray(); return a[a.Length / 2]; }
            if (Median(tail.Select(s => (long)s.Handles)) - Median(middle.Select(s => (long)s.Handles)) > 32) throw new Exception("Sustained handle growth");
            if (Median(tail.Select(s => s.PrivateBytes)) - Median(middle.Select(s => s.PrivateBytes)) > 256L * 1024 * 1024) throw new Exception("Sustained private-memory growth; inspect Valgrind report");
        }
    }
    public static void Large(string path)
    {
        // 4097 MiB of real, uncompressed payload: offsets must exceed uint.MaxValue.
        var data = new byte[1024 * 1024];
        try
        {
            using (var w = new McapWriter(path, new() { Compression = McapCompression.None, ChunkSize = 4 * 1024 * 1024 }))
            {
                var c = w.RegisterChannel("large", "raw");
                for (uint i = 0; i < 4097; i++) { BitConverter.TryWriteBytes(data.AsSpan(), i); w.WriteMessage(new McapMessageHeader(c, i, i, i), data); }
                w.Complete();
            }
            if (new FileInfo(path).Length <= uint.MaxValue) throw new Exception("Large fixture too small");
            var r = new McapReader(path); r.Validate();
            using var s = r.OpenIndexedMessages(new() { StartTime = 4096 });
            if (s.ReadNext(data, out var h, out var n) != McapReadStatus.Message || h.Sequence != 4096 || n != (ulong)data.Length || BitConverter.ToUInt32(data) != 4096) throw new Exception("64-bit index mismatch");
            if (s.ReadNext(data, out _, out _) != McapReadStatus.EndOfStream) throw new Exception("Unexpected tail");
        }
        finally { File.Delete(path); }
    }
}
