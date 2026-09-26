using System.Text.Json;

namespace Fizzy.McapSharp;

/// <summary>Opens an independent native file reader for each enumeration. Records own their managed buffers.</summary>
public sealed class McapReader
{
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private readonly string path;
    public McapReader(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        Native.EnsureAvailable(); this.path = Path.GetFullPath(path);
    }
    /// <summary>Reads in file/chunk order. StartTime is inclusive; EndTime is exclusive. Queries validate touched chunks, not the whole file.</summary>
    public IEnumerable<McapMessage> ReadMessages(McapQuery? query = null)
    {
        query ??= new();
        if (query.StartTime.HasValue && query.EndTime.HasValue && query.StartTime > query.EndTime)
            throw new ArgumentException("StartTime must not exceed EndTime.", nameof(query));
        foreach (var item in Read("messages", query))
        {
            using var json = item.Json!;
            var value = json.RootElement;
            yield return new(value.GetProperty("channel").Deserialize<McapChannel>(JsonOptions)!, value.GetProperty("logTime").GetUInt64(), value.GetProperty("publishTime").GetUInt64(), value.GetProperty("sequence").GetUInt32(), item.Data);
        }
    }
    /// <summary>Delivers recoverable messages until the first malformed record. Always check IsComplete; callback errors propagate.</summary>
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        ArgumentNullException.ThrowIfNull(accept);
        ulong count = 0;
        using var iterator = Read("messages", null, recovery: true).GetEnumerator();
        while (true)
        {
            bool available;
            try { available = iterator.MoveNext(); }
            catch (McapException error) { return new(count, false, error.Message); }
            if (!available) break;
            var item = iterator.Current;
            using var json = item.Json!; var v = json.RootElement;
            accept(new(v.GetProperty("channel").Deserialize<McapChannel>(JsonOptions)!, v.GetProperty("logTime").GetUInt64(), v.GetProperty("publishTime").GetUInt64(), v.GetProperty("sequence").GetUInt32(), item.Data));
            count++;
        }
        try { Validate(); return new(count, true, null); }
        catch (McapException error) { return new(count, false, error.Message); }
    }
    public IEnumerable<McapSchema> ReadSchemas()
    {
        foreach (var item in Read("schemas", null))
        {
            using var json = item.Json!; var v = json.RootElement;
            yield return new(v.GetProperty("id").GetUInt16(), v.GetProperty("name").GetString()!, v.GetProperty("encoding").GetString()!, item.Data);
        }
    }
    public IEnumerable<McapChannel> ReadChannels()
    {
        var schemas = ReadSchemas().ToDictionary(x => x.Id);
        foreach (var item in Read("channels", null))
        {
            using var json = item.Json!; var v = json.RootElement;
            var schemaId = v.GetProperty("schemaId").GetUInt16();
            yield return new(v.GetProperty("id").GetUInt16(), v.GetProperty("topic").GetString()!, v.GetProperty("messageEncoding").GetString()!, schemaId == 0 ? null : schemas[schemaId], v.GetProperty("metadata").Deserialize<Dictionary<string,string>>()!);
        }
    }
    public IEnumerable<McapMetadata> ReadMetadata()
    {
        foreach (var item in Read("metadata", null))
        { using var json = item.Json!; yield return json.RootElement.Deserialize<McapMetadata>(JsonOptions)!; }
    }
    public IEnumerable<McapAttachment> ReadAttachments()
    {
        foreach (var item in Read("attachments", null))
        {
            using var json = item.Json!; var v = json.RootElement;
            yield return new(v.GetProperty("name").GetString()!, v.GetProperty("mediaType").GetString()!, v.GetProperty("logTime").GetUInt64(), v.GetProperty("createTime").GetUInt64(), item.Data);
        }
    }
    /// <summary>Scans the entire file, checking present chunk, attachment, data and summary CRCs, framing and final magic.</summary>
    public ulong Validate()
    {
        var request = Native.Request(new { path });
        var status = Native.fm_validate(request, (nuint)request.Length, out var result);
        var response = Native.Consume(status, result); response.Json?.Dispose(); return response.Value;
    }
    private IEnumerable<(JsonDocument? Json, byte[] Data, ulong Value)> Read(string mode, McapQuery? query, bool recovery = false)
    {
        var request = Native.Request(new { path, mode, recovery, topic = query?.Topic, start = query?.StartTime, end = query?.EndTime });
        var status = Native.fm_reader_open(request, (nuint)request.Length, out var pointer, out var result);
        Native.Consume(status, result).Json?.Dispose();
        using var handle = new ReaderHandle(pointer);
        while (true)
        {
            status = Native.fm_reader_next(handle, out result);
            var item = Native.Consume(status, result);
            if (status == 1) { item.Json?.Dispose(); yield break; }
            yield return item;
        }
    }
}
