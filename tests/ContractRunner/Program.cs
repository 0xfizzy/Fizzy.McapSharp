using Fizzy.McapSharp;
using System.Collections;
using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Reflection;
using System.Runtime.InteropServices;

if (args.Length < 2) throw new ArgumentException("Expected command and input path");
switch (args[0])
{
    case "stream":
        using (var reader = new McapBufferReader(File.ReadAllBytes(args[1]), McapBufferReadMode.FlattenChunks))
            Print(new { records = reader.ReadRecords().Where(r => r.Opcode != 7).Select(r => Canon.Record(((McapOpcode)r.Opcode).ToString(), McapRecords.Parse(r.Opcode, r.Data))).ToArray() });
        break;
    case "indexed":
        using (var snapshot = new McapIndexSnapshot(File.ReadAllBytes(args[1])))
        using (var records = snapshot.OpenSummaryRecords())
        {
            var all = records.ReadRecords().ToArray();
            Print(new {
                schemas = all.Where(r => r.Opcode == 3).Select(r => (McapSchema)McapRecords.Parse(3, r.Data)).OrderBy(r => r.Id).Select(r => Canon.Record("Schema", r)).ToArray(),
                channels = all.Where(r => r.Opcode == 4).Select(r => (McapChannelRecord)McapRecords.Parse(4, r.Data)).OrderBy(r => r.Id).Select(r => Canon.Record("Channel", r)).ToArray(),
                messages = Canon.Indexed(args[1]),
                statistics = all.Where(r => r.Opcode == 11).Select(r => Canon.Record("Statistics", McapRecords.Parse(11, r.Data))).ToArray()
            });
        }
        break;
    case "write": Canon.Write(args[1], args[2], args.Length > 3 ? Enum.Parse<McapCompression>(args[3]) : McapCompression.None); break;
    case "check": await Contracts.Check(args[1], args[2]); break;
    case "exchange": Contracts.Exchange(args[1]); break;
    case "load-failure":
        NativeLibrary.SetDllImportResolver(typeof(McapFileReader).Assembly, (name, assembly, search) =>
            args[1] == "missing" ? throw new DllNotFoundException("Isolated missing native asset") : NativeLibrary.Load(args[2]));
        try { _ = new McapFileReader("unused"); throw new Exception("Expected load rejection"); }
        catch (DllNotFoundException) when (args[1] == "missing") { }
        catch (McapException e) when (args[1] == "abi" && e.Message.Contains("ABI")) { }
        break;
    case "exports":
        var library = NativeLibrary.Load(Path.GetFullPath(args[1]));
        try
        {
            var imports = typeof(McapFileReader).Assembly.GetTypes().SelectMany(t => t.GetMethods(BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic));
            foreach (var method in imports)
                if (method.GetCustomAttribute<DllImportAttribute>() is { Value: "fizzy_mcap_native" } attribute)
                    if (!NativeLibrary.TryGetExport(library, attribute.EntryPoint ?? method.Name, out _)) throw new Exception("Missing export: " + method.Name);
        }
        finally { NativeLibrary.Free(library); }
        break;
    case "probe": Contracts.Probe(args[1]); break;
    case "large": Contracts.Large(args[1]); break;
    case "lifecycle": await Contracts.Lifecycle(args[1], int.Parse(args[2])); break;
    default: throw new ArgumentException("Unknown command");
}
static void Print(object value) => Console.WriteLine(JsonSerializer.Serialize(value));

static class Canon
{
    public static object[] Indexed(string path)
    {
        using var file = File.OpenRead(path);
        using var summary = McapSansIoReader.CreateSummary(new() { FileSize = (ulong)file.Length });
        var buffer = new byte[1024 * 1024];
        while (summary.NextEvent(buffer, out var e) != McapReadStatus.EndOfStream)
        {
            if (e.Kind == McapReadEventKind.Seek) summary.NotifySeeked((ulong)file.Seek(unchecked((long)e.Offset), e.Origin));
            else { int n = file.Read(buffer, 0, (int)Math.Min((ulong)buffer.Length, e.Length)); summary.SupplyInput(buffer.AsSpan(0, n)); }
        }
        using var indexed = summary.CreateIndexed();
        var messages = new List<object>();
        while (true)
        {
            var status = indexed.NextEvent(buffer, out var e);
            if (status == McapReadStatus.EndOfStream) break;
            if (e.Length > (ulong)buffer.Length) buffer = new byte[checked((int)e.Length)];
            if (status == McapReadStatus.BufferTooSmall) continue;
            if (e.Kind == McapReadEventKind.ReadChunk) { file.Position = (long)e.Offset; file.ReadExactly(buffer.AsSpan(0, (int)e.Length)); indexed.SupplyInput(buffer.AsSpan(0, (int)e.Length), e.Offset); }
            else messages.Add(Record("Message", new McapMessageRecord(e.Header, buffer.AsSpan(0, (int)e.Length).ToArray())));
        }
        return messages.ToArray();
    }
    static string Snake(string value) => Regex.Replace(value, "(?<!^)([A-Z])", "_$1").ToLowerInvariant();
    public static object Record(string type, object value)
    {
        var fields = new SortedDictionary<string, object>(StringComparer.Ordinal);
        void Add(object source)
        {
            foreach (var p in source.GetType().GetProperties())
            {
                if (type == "Attachment" && p.Name == "Crc") continue; // Official JSON omits attachment CRC.
                var v = p.GetValue(source)!;
                if (p.Name == "Header") { Add(v); continue; }
                fields[p.Name == "Values" ? "metadata" : Snake(p.Name)] = Normalize(v);
            }
        }
        Add(value);
        return new { type, fields = fields.Select(p => new object[] { p.Key, p.Value }).ToArray() };
    }
    static object Normalize(object v) => v switch
    {
        string s => s,
        byte[] b => b.Select(x => x.ToString(CultureInfo.InvariantCulture)).ToArray(),
        IDictionary d => d.Keys.Cast<object>().ToDictionary(k => k.ToString()!, k => Normalize(d[k]!)),
        byte or ushort or uint or ulong or int or long => Convert.ToString(v, CultureInfo.InvariantCulture)!,
        _ => throw new NotSupportedException(v.GetType().Name)
    };
    public static object Message(McapMessage m) => Record("Message", new McapMessageRecord(new(m.Channel.Id, m.Sequence, m.LogTime, m.PublishTime), m.Data));
    public static Dictionary<string, JsonElement> Fields(JsonElement r) => r.GetProperty("fields").EnumerateArray().ToDictionary(x => x[0].GetString()!, x => x[1]);
    public static void Write(string input, string output, McapCompression compression)
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(input));
        var root = doc.RootElement;
        var features = root.GetProperty("meta").GetProperty("variant").GetProperty("features").EnumerateArray().Select(x => x.GetString()).ToHashSet();
        if (features.Contains("pad")) throw new NotSupportedException("Upstream writer cannot emit record padding");
        using var w = new McapWriter(output, new() {
            Compression = compression, Library = "", DisableSeeking = true,
            UseChunks = features.Contains("ch"), EmitSummaryRecords = false,
            EmitSummaryOffsets = features.Contains("sum"), EmitStatistics = features.Contains("st"),
            EmitMessageIndexes = features.Contains("mx"), EmitChunkIndexes = features.Contains("chx"),
            EmitAttachmentIndexes = features.Contains("ax"), EmitMetadataIndexes = features.Contains("mdx"),
            RepeatSchemas = features.Contains("rsh"), RepeatChannels = features.Contains("rch")
        });
        foreach (var record in root.GetProperty("records").EnumerateArray())
        {
            var f = Fields(record);
            string S(string k) => f[k].GetString()!;
            ulong U(string k) => ulong.Parse(S(k), CultureInfo.InvariantCulture);
            byte[] B() => f["data"].EnumerateArray().Select(x => byte.Parse(x.GetString()!, CultureInfo.InvariantCulture)).ToArray();
            Dictionary<string, string> D() => f["metadata"].Deserialize<Dictionary<string, string>>()!;
            switch (record.GetProperty("type").GetString())
            {
                case "Schema": w.RegisterSchema((ushort)U("id"), S("name"), S("encoding"), B()); break;
                case "Channel": w.RegisterChannel((ushort)U("id"), S("topic"), S("message_encoding"), (ushort)U("schema_id"), D()); break;
                case "Message": w.WriteMessage(new McapMessageHeader((ushort)U("channel_id"), (uint)U("sequence"), U("log_time"), U("publish_time")), B()); break;
                case "Metadata": w.WriteMetadata(S("name"), D()); break;
                case "Attachment": w.WriteAttachment(S("name"), S("media_type"), U("log_time"), U("create_time"), B()); break;
            }
        }
        w.Complete();
    }
}
