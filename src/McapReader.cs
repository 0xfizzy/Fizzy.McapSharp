namespace Fizzy.McapSharp;
/// <summary>File reader factory. Each session owns its native handle and file mapping.</summary>
public sealed class McapReader
{
    readonly string path;
    public McapReader(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        Native.EnsureAvailable();
        this.path = Path.GetFullPath(path);
    }

    public McapReadSession OpenMessages(McapQuery? query = null) => new(path, null, query, true, McapRecordMode.ExpandChunks, false);
    public McapReadSession OpenRecords(McapRecordMode mode = McapRecordMode.ExpandChunks) => new(path, null, null, false, mode, false);
    public static McapReadSession OpenMessages(Stream stream, McapQuery? query = null, bool leaveOpen = false) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), query, true, McapRecordMode.ExpandChunks, leaveOpen);
    public static McapReadSession OpenRecords(Stream stream, McapRecordMode mode = McapRecordMode.ExpandChunks, bool leaveOpen = false) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), null, false, mode, leaveOpen);
    public IEnumerable<McapMessage> ReadMessages(McapQuery? query = null)
    {
        using var session = OpenMessages(query);
        foreach (var m in session.ReadMessages())
            yield return m;
    }

    public IEnumerable<McapSchema> ReadSchemas()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadSchemas())
            yield return item;
    }

    public IEnumerable<McapChannel> ReadChannels()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadChannels())
            yield return item;
    }

    public IEnumerable<McapMetadata> ReadMetadata()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadMetadata())
            yield return item;
    }

    public IEnumerable<McapAttachment> ReadAttachments()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadAttachments())
            yield return item;
    }

    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        using var s = OpenMessages();
        return s.RecoverMessages(accept);
    }

    public McapSummary? GetSummary()
    {
        using var s = OpenRecords();
        return s.GetSummary();
    }

    public ulong Validate()
    {
        var req = Native.Request(new { path });
        int status = Native.fm_validate(req, (nuint)req.Length, out var r);
        var x = Native.Consume(status, r);
        x.Json?.Dispose();
        return x.Value;
    }
}
