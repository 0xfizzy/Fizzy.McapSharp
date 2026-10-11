namespace Fizzy.McapSharp;

public sealed partial class McapReaderFactory
{
    /// <summary>Opens independent file-order message reading with FullScan scope, or StrictFullScan when strict options are supplied. Filtering does not narrow validation. Dispose the reader to release its input.</summary>
    public McapMessageReader OpenMessages(McapMessageFilter? filter = null, McapReaderOptions? options = null)
        => new(new ReadSessionCore(path, null, (filter ?? new()).ToQuery(), true, RecordMode.ExpandChunks, false, options, sequentialOnly: true));
    /// <summary>Opens file-order messages over a readable Stream with FullScan scope, or StrictFullScan under strict options. Filtering does not narrow validation. Owns the Stream unless leaveOpen is true.</summary>
    public static McapMessageReader OpenMessages(Stream stream, McapMessageFilter? filter = null, bool leaveOpen = false, McapReaderOptions? options = null)
        => new(new ReadSessionCore(null, stream ?? throw new ArgumentNullException(nameof(stream)), (filter ?? new()).ToQuery(), true, RecordMode.ExpandChunks, leaveOpen, options, sequentialOnly: true));
    /// <summary>Opens expanded file records with declaration-reference validation independently of CRC options. ValidationCapability is FullScan, or StrictFullScan with strict options.</summary>
    public McapChunkFlattener OpenRecords(McapReaderOptions? options = null)
        => new(OpenRecordSession(options: ExpandedOptions(options)));
    /// <summary>Opens expanded records with FullScan scope, or StrictFullScan under strict options. Owns the Stream unless leaveOpen is true.</summary>
    public static McapChunkFlattener OpenRecords(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
        => new(OpenRecordSession(stream, leaveOpen: leaveOpen, options: ExpandedOptions(options)));
    /// <summary>Opens top-level records without expanding chunks. Does not validate declarations inside chunks.</summary>
    public McapLinearReader OpenLinear(McapReaderOptions? options = null)
        => new(OpenRecordSession(RecordMode.TopLevel, options));
    /// <summary>Opens top-level Stream records without expanding chunks. Owns the Stream unless leaveOpen is true.</summary>
    public static McapLinearReader OpenLinear(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
        => new(OpenRecordSession(stream, RecordMode.TopLevel, leaveOpen, options));
    /// <summary>Opens indexed messages without scan or buffered-sort fallback. ValidationCapability is PartialInput; success is not full-file validation.</summary>
    public McapMessageReader OpenIndexedMessages(McapIndexedQuery? query = null)
        => new(OpenIndexedSession((query ?? new()).ToQuery(), new() { RecordLengthLimit = query?.RecordLengthLimit }));
    /// <summary>Executes an explicit ordered query using indexes where possible. Buffered fallback follows the query policy; omitted and empty queries default to ascending log time. ValidationCapability reports the actual execution path; indexed and sorted results are PartialInput.</summary>
    public McapMessageReader QueryMessages(McapQuery? query = null, McapReaderOptions? options = null)
        => new(OpenMessageSession(query ?? new(), options));
    /// <summary>Executes an explicit ordered query over a Stream. Owns the Stream unless leaveOpen is true; fallback sorting may consume and retain all selected messages before returning. Indexed and sorted results have PartialInput validation capability.</summary>
    public static McapMessageReader QueryMessages(Stream stream, McapQuery? query = null, bool leaveOpen = false, McapReaderOptions? options = null)
        => new(OpenMessageSession(stream, query ?? new(), leaveOpen, options));
    /// <summary>Enumerates independent owned messages from an explicit ordered query. Enumeration owns and disposes its reader.</summary>
    public IEnumerable<McapMessage> ReadQueryMessages(McapQuery? query = null)
    {
        var reader = QueryMessages(query);
        foreach (var message in EnumerateAndDispose(reader, reader.ReadMessages())) yield return message;
    }
    /// <summary>Copies the complete file into an independent indexed reader; dispose the returned reader. Cache allowance excludes input storage.</summary>
    public McapIndexedReader CreateCopiedIndexedReader(McapIndexedReaderOptions? options = null)
        => CreateSnapshot(OpenRecordSession(), options);
    /// <summary>Copies the complete seekable source from its initial MCAP position into an independent indexed reader; dispose the returned reader. Owns the Stream unless leaveOpen is true. Copy and cleanup failures are reported.</summary>
    public static McapIndexedReader CreateCopiedIndexedReader(Stream stream, McapIndexedReaderOptions? options = null, bool leaveOpen = false)
        => CreateSnapshot(OpenRecordSession(stream, leaveOpen: leaveOpen), options);
    /// <summary>Opens raw-record random access without copying the complete file. Dispose the reader to release its input.</summary>
    public McapRecordAccessor OpenRecordAccessor(ulong? recordLengthLimit = null)
        => new(OpenRecordSession(options: new() { RecordLengthLimit = recordLengthLimit }));
    /// <summary>Opens raw-record random access over a readable, seekable Stream. Owns the Stream unless leaveOpen is true; offsets start at its initial position.</summary>
    public static McapRecordAccessor OpenRecordAccessor(Stream stream, bool leaveOpen = false, ulong? recordLengthLimit = null)
    {
        ArgumentNullException.ThrowIfNull(stream);
        if (!stream.CanSeek) throw new ArgumentException("Random access requires a seekable Stream.", nameof(stream));
        return new(OpenRecordSession(stream, leaveOpen: leaveOpen, options: new() { RecordLengthLimit = recordLengthLimit }));
    }
    /// <summary>Scans a Stream and yields independent owned results. Enumeration owns the session and Stream unless leaveOpen is true; skipped records are not copied into managed objects.</summary>
    public static IEnumerable<McapSchema> ReadSchemas(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
    {
        var reader = OpenRecordSession(stream, leaveOpen: leaveOpen, options: ExpandedOptions(options));
        foreach (var item in EnumerateAndDispose(reader, reader.ReadSchemas())) yield return item;
    }
    /// <summary>Scans a Stream and yields independent owned results. Enumeration owns the session and Stream unless leaveOpen is true; skipped records are not copied into managed objects.</summary>
    public static IEnumerable<McapChannel> ReadChannels(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
    {
        var reader = OpenRecordSession(stream, leaveOpen: leaveOpen, options: ExpandedOptions(options));
        foreach (var item in EnumerateAndDispose(reader, reader.ReadChannels())) yield return item;
    }
    /// <summary>Scans a Stream and yields independent owned results. Enumeration owns the session and Stream unless leaveOpen is true; skipped records are not copied into managed objects.</summary>
    public static IEnumerable<McapMetadata> ReadMetadata(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
    {
        var reader = OpenRecordSession(stream, leaveOpen: leaveOpen, options: ExpandedOptions(options));
        foreach (var item in EnumerateAndDispose(reader, reader.ReadMetadata())) yield return item;
    }
    /// <summary>Scans a Stream and yields independent owned results. Enumeration owns the session and Stream unless leaveOpen is true; skipped records are not copied into managed objects.</summary>
    public static IEnumerable<McapAttachment> ReadAttachments(Stream stream, bool leaveOpen = false, McapReaderOptions? options = null)
    {
        var reader = OpenRecordSession(stream, leaveOpen: leaveOpen, options: ExpandedOptions(options));
        foreach (var item in EnumerateAndDispose(reader, reader.ReadAttachments())) yield return item;
    }
    static McapReaderOptions? ExpandedOptions(McapReaderOptions? options)
    {
        if (options?.EmitChunks == true) throw new ArgumentException("Use OpenLinear to expose encoded chunks.", nameof(options));
        return options;
    }
    /// <summary>Reads an independent summary. Seekable input uses random access; non-seekable input is scanned to EOF. Owns the Stream unless leaveOpen is true; success alone does not establish full-file validation.</summary>
    public static McapSummary? GetSummary(Stream stream, bool leaveOpen = false)
    {
        ArgumentNullException.ThrowIfNull(stream);
        using var reader = OpenRecordSession(stream, leaveOpen: leaveOpen);
        try
        {
            if (!stream.CanSeek)
            {
                byte[] buffer = new byte[65536];
                while (true)
                {
                    var status = reader.ReadNextRecord(buffer, out _, out var length);
                    if (status == McapReadStatus.EndOfStream) break;
                    if (status == McapReadStatus.BufferTooSmall) buffer = new byte[checked((int)length)];
                }
            }
            return reader.GetSummary();
        }
        catch (Exception operation)
        {
            try { reader.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }
    /// <summary>Delivers a strict file-order valid prefix from a Stream. Parsing errors are retained in the result; Stream and callback exceptions propagate and terminate reading. Owns the Stream unless leaveOpen is true.</summary>
    public static McapRecoveryResult RecoverMessages(Stream stream, Action<McapMessage> accept, bool leaveOpen = false)
    {
        using var reader = OpenMessages(stream, leaveOpen: leaveOpen, options: McapReaderOptions.Strict);
        try { return reader.RecoverMessages(accept); }
        catch (Exception operation)
        {
            try { reader.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }
    static McapIndexedReader CreateSnapshot(ReadSessionCore reader, McapIndexedReaderOptions? options)
    {
        McapIndexedReader snapshot;
        try { snapshot = reader.CreateCopiedIndexedReader(options); }
        catch (Exception operation)
        {
            try { reader.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
        try { reader.Dispose(); }
        catch (Exception cleanup)
        {
            // The child has not been published: this boundary must release its ownership.
            try { snapshot.Dispose(); }
            catch (Exception childCleanup) { throw new AggregateException(cleanup, childCleanup); }
            throw;
        }
        return snapshot;
    }
}
