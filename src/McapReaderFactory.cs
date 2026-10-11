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
    internal ReadSessionCore OpenMessageSession(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query, true, RecordMode.ExpandChunks, false, options);
    /// <summary>Opens an independent raw-record session. ExpandChunks is the default. Dispose the returned session.</summary>
    internal ReadSessionCore OpenRecordSession(RecordMode mode = RecordMode.ExpandChunks, McapReaderOptions? options = null) => new(path, null, null, false, mode, false, options);
    /// <summary>Opens one exclusive session over a readable Stream; owns the Stream unless leaveOpen is true. A null query uses file order. Dispose the session before reusing the Stream.</summary>
    internal static ReadSessionCore OpenMessageSession(Stream stream, McapQuery? query = null, bool leaveOpen = false, McapReaderOptions? options = null) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), query, true, RecordMode.ExpandChunks, leaveOpen, options);
    /// <summary>Opens one exclusive raw-record session over a readable Stream; owns the Stream unless leaveOpen is true. Dispose the session before reusing the Stream.</summary>
    internal static ReadSessionCore OpenRecordSession(Stream stream, RecordMode mode = RecordMode.ExpandChunks, bool leaveOpen = false, McapReaderOptions? options = null) => new(null, stream ?? throw new ArgumentNullException(nameof(stream)), null, false, mode, leaveOpen, options);
    /// <summary>Returns independent mutable results, copying payloads and mutable declarations.
    /// Use caller-buffer, visitor or lease delivery when independent result objects are not needed.</summary>
    public IEnumerable<McapMessage> ReadMessages(McapMessageFilter? filter = null)
    {
        var session = OpenMessages(filter);
        foreach (var m in EnumerateAndDispose(session, session.ReadMessages()))
            yield return m;
    }

    /// <summary>Opens an independent scan and copies each schema once per ID. Enumeration owns and disposes its session.</summary>
    public IEnumerable<McapSchema> ReadSchemas()
    {
        var session = OpenRecordSession();
        foreach (var item in EnumerateAndDispose(session, session.ReadSchemas()))
            yield return item;
    }

    /// <summary>Opens an independent scan and copies each channel and its schema once per ID.</summary>
    public IEnumerable<McapChannel> ReadChannels()
    {
        var session = OpenRecordSession();
        foreach (var item in EnumerateAndDispose(session, session.ReadChannels()))
            yield return item;
    }

    /// <summary>Opens an independent scan and returns owned metadata records.</summary>
    public IEnumerable<McapMetadata> ReadMetadata()
    {
        var session = OpenRecordSession();
        foreach (var item in EnumerateAndDispose(session, session.ReadMetadata()))
            yield return item;
    }

    /// <summary>Opens an independent scan and returns attachments with independent payload arrays.</summary>
    public IEnumerable<McapAttachment> ReadAttachments()
    {
        var session = OpenRecordSession();
        foreach (var item in EnumerateAndDispose(session, session.ReadAttachments()))
            yield return item;
    }

    /// <summary>Opens a strict file-order scan, delivers the valid prefix and retains the original structured parse error in the result. The accept callback receives independently owned messages; its exceptions and Stream exceptions propagate, including McapException. The temporary session is disposed when this method returns or throws.</summary>
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        using var s = OpenMessageSession(options: McapReaderOptions.Strict);
        try { return s.RecoverMessages(accept); }
        catch (Exception operation)
        {
            try { s.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }

    /// <summary>Reads an independent summary snapshot, or null if absent. Successful summary access does not validate the entire file.</summary>
    public McapSummary? GetSummary()
    {
        using var s = OpenRecordSession();
        try { return s.GetSummary(); }
        catch (Exception operation)
        {
            try { s.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
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
    internal ReadSessionCore OpenIndexedSession(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query ?? new(), true, RecordMode.ExpandChunks, false, options, true);
}
