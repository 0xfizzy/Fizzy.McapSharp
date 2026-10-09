namespace Fizzy.McapSharp;
/// <summary>Reusable reader factory with instance methods for a file path and static methods for Streams; construction validates the path but does not open the file. This factory owns no disposable resources. Each opened session or enumeration owns an independent native reader and its input.</summary>
public sealed partial class McapReaderFactory
{
    readonly string path;
    /// <summary>Creates a reusable factory for an absolute-normalized path without opening the input file.</summary>
    public McapReaderFactory(string path)
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

    /// <summary>Opens an independent scan and copies each schema once per ID. Enumeration owns and disposes its session.</summary>
    public IEnumerable<McapSchema> ReadSchemas()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadSchemas())
            yield return item;
    }

    /// <summary>Opens an independent scan and copies each channel and its schema once per ID.</summary>
    public IEnumerable<McapChannel> ReadChannels()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadChannels())
            yield return item;
    }

    /// <summary>Opens an independent scan and returns owned metadata records.</summary>
    public IEnumerable<McapMetadata> ReadMetadata()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadMetadata())
            yield return item;
    }

    /// <summary>Opens an independent scan and returns attachments with independent payload arrays.</summary>
    public IEnumerable<McapAttachment> ReadAttachments()
    {
        using var session = OpenRecords();
        foreach (var item in session.ReadAttachments())
            yield return item;
    }

    /// <summary>Opens a strict file-order scan, delivers the valid prefix and retains the original structured parse error in the result. The accept callback receives independently owned messages; its exceptions and Stream exceptions propagate, including McapException. The temporary session is disposed when this method returns or throws.</summary>
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        using var s = OpenMessages(options: McapReaderOptions.Strict);
        return s.RecoverMessages(accept);
    }

    /// <summary>Reads an independent summary snapshot, or null if absent. Successful summary access does not validate the entire file.</summary>
    public McapSummary? GetSummary()
    {
        using var s = OpenRecords();
        return s.GetSummary();
    }

    /// <summary>Strictly scans the entire file and returns its record count, throwing on format or integrity failures.</summary>
    public ulong Validate()
    {
        var req = Native.Request(new { path });
        int status = Native.fm_validate(req, (nuint)req.Length, out var r);
        var x = Native.Consume(status, r);
        x.Json?.Dispose();
        return x.Value;
    }
}

public sealed partial class McapReaderFactory
{
    /// <summary>Opens an indexed-only message session. Never falls back to scanning or buffered sorting; query AllowBufferedSort and MaxBufferedSortBytes do not apply. A successful query is not full-file validation.</summary>
    public McapReadSession OpenIndexedMessages(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query ?? new(), true, McapRecordMode.ExpandChunks, false, options, true);
}
