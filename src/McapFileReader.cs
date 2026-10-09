namespace Fizzy.McapSharp;
/// <summary>Reusable file-path factory; construction validates the path but does not open the file. This factory owns no disposable resources. Each opened session or enumeration owns an independent native reader and its input.</summary>
public sealed partial class McapFileReader
{
    readonly string path;
    public McapFileReader(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        Native.EnsureAvailable();
        this.path = Path.GetFullPath(path);
    }

    /// <summary>Opens an independent message session. A null query scans in file order; an explicit query chooses its Order. Dispose the returned session.</summary>
    public McapReadSession OpenMessages(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query, true, McapRecordMode.ExpandChunks, false, options);
    /// <summary>Opens an independent raw-record session. ExpandChunks is the default. Dispose the returned session.</summary>
    public McapReadSession OpenRecords(McapRecordMode mode = McapRecordMode.ExpandChunks, McapReaderOptions? options = null) => new(path, null, null, false, mode, false, options);
    /// <summary>Opens one exclusive session over a readable Stream; owns the Stream unless leaveOpen is true. A null query uses file order. Dispose the session before reusing the Stream.</summary>
    public static McapReadSession OpenMessages(Stream stream, McapQuery? query = null, bool leaveOpen = false, McapReaderOptions? options = null) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), query, true, McapRecordMode.ExpandChunks, leaveOpen, options);
    /// <summary>Opens one exclusive raw-record session over a readable Stream; owns the Stream unless leaveOpen is true. Dispose the session before reusing the Stream.</summary>
    public static McapReadSession OpenRecords(Stream stream, McapRecordMode mode = McapRecordMode.ExpandChunks, bool leaveOpen = false, McapReaderOptions? options = null) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), null, false, mode, leaveOpen, options);
    /// <summary>Returns independent mutable results, copying payloads and mutable declarations.
    /// Use caller-buffer, visitor or lease delivery when independent result objects are not needed.</summary>
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
        using var s = OpenMessages(options: McapReaderOptions.Strict);
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
