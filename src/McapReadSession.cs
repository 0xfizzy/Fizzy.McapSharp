using System.Text.Json;

namespace Fizzy.McapSharp;
/// <summary>Owns one native reader. Calls are serialized and cannot be reentered from Stream callbacks.</summary>
public sealed partial class McapReadSession : IDisposable
{
    readonly ReaderHandle handle;
    readonly bool seekable;
    readonly object gate = new();
    readonly BorrowedReadSink borrowed = new();
    bool disposed, failed, ended, fullyValidated;
    readonly bool messages;
    readonly bool topLevel;
    readonly bool strict;
    /// <summary>True after reaching the end of this scan without failure; does not imply full-file validation.</summary>
    public bool IsScanComplete { get { lock (gate) { borrowed.CheckReentry(); return ended && !failed; } } }
    /// <summary>True only after a strict complete expanded scan validates the entire file; indexed or non-strict success is insufficient.</summary>
    public bool IsFullyValidated { get { lock (gate) { borrowed.CheckReentry(); return ended && !failed && fullyValidated; } } }
    ulong scannedRecordCount;
    /// <summary>Number of records reported by the completed native scan; updated at EOF, not a live progress counter.</summary>
    public ulong ScannedRecordCount { get { lock (gate) { borrowed.CheckReentry(); return scannedRecordCount; } } private set => scannedRecordCount = value; }

    internal unsafe McapReadSession(string? path, Stream? stream, McapQuery? query, bool messages, McapRecordMode mode, bool leaveOpen, McapReaderOptions? options = null, bool indexedOnly = false)
    {
        if (!Enum.IsDefined(mode))
            throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        if (query?.StartTime > query?.EndTime)
            throw new ArgumentException("StartTime must not exceed EndTime.", nameof(query));
        options ??= new();
        strict = options.IsStrict;
        if (query is not null && !Enum.IsDefined(query.Order)) throw new ArgumentOutOfRangeException(nameof(query));
        if (query?.Topic is not null && query.Topics is not null) throw new ArgumentException("Specify Topic or Topics, not both.", nameof(query));
        this.messages = messages;
        topLevel = mode == McapRecordMode.TopLevel || options.EmitChunks;
        if (messages && topLevel) throw new ArgumentException("Messages require expanded chunks.", nameof(options));
        seekable = stream?.CanSeek ?? true;
        var request = Native.Request(new { path, messages, topLevel, topic = query?.Topic, start = query?.StartTime, end = query?.EndTime, topics = query?.Topics, order = (int)(query?.Order ?? McapReadOrder.File), allowBufferedSort = query?.AllowBufferedSort ?? true, indexedOnly, options, maxBufferedSortBytes = query?.MaxBufferedSortBytes, recordLengthLimit = options.RecordLengthLimit });
        StreamBridge? bridge = stream is null ? null : new(stream, false, leaveOpen);
        ReaderHandle? opened = null;
        try
        {
            var cb = bridge?.Callbacks ?? default;
            int status = Native.fm_reader_open(request, (nuint)request.Length, bridge is null ? null : &cb, out var p, out var r);
            if (p != IntPtr.Zero) opened = new(p, bridge);
            Native.ConsumeReader(status, r, bridge).Json?.Dispose();
            if (status == Protocol.ReaderOpenStatus.BufferedSortRequired) throw new NotSupportedException("Time ordering requires buffered sorting, which this query disables.");

            handle = opened!;
        }
        catch (Exception operation)
        {
            try { if (opened is not null) opened.Dispose(); else bridge?.Release(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }

    void Check()
    {
        borrowed.CheckReentry(); handle.Bridge?.CheckReentry();
        ObjectDisposedException.ThrowIf(disposed, this);
        if (failed)
            throw new InvalidOperationException("Reader failed; open a new session.");
    }

    /// <summary>Copies one message payload into caller storage. BufferTooSmall reports the required byte length and preserves the pending message for retry; EOF is not proof of complete validation.</summary>
    public McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong requiredLength)
    {
        if (!messages)
            throw new InvalidOperationException("This is a record session.");
        return Read(destination, out header, out _, out requiredLength);
    }

    /// <summary>Copies one raw record body into caller storage. BufferTooSmall retains the record and reports required capacity. The returned opcode identifies the body; length excludes the record header.</summary>
    public McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong requiredLength)
    {
        if (messages)
            throw new InvalidOperationException("This is a message session.");
        return Read(destination, out _, out opcode, out requiredLength);
    }

    unsafe McapReadStatus Read(Span<byte> destination, out McapMessageHeader header, out byte opcode, out ulong requiredLength)
    {
        lock (gate)
        {
            Check();
            try
            {
                fixed (byte* p = destination)
                {
                    int status = Native.fm_reader_next(handle, p, (nuint)destination.Length, out var h, out opcode, out var r);
                    header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime);
                    requiredLength = r.Value;
                    if (status < Protocol.Status.Success)
                    {
                        var error = Native.ConsumeError(r);
                        handle.Bridge?.ThrowIfError();
                        throw error;
                    }

                    if (status == Protocol.Status.End)
                    {
                        ended = true;
                        fullyValidated = strict && h.Reserved == 0 && !topLevel;
                        ScannedRecordCount = r.Value;
                        requiredLength = 0;
                        header = default;
                        opcode = 0;
                        return McapReadStatus.EndOfStream;
                    }

                    return status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
                }
            }
            catch
            {
                failed = true;
                throw;
            }
        }
    }

    /// <summary>Copies a schema already encountered by this session. Unknown IDs fail lookup without advancing or terminating the session.</summary>
    public McapSchema GetSchema(ushort id)
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_reader_describe(handle, Protocol.DeclarationKind.Schema, id, out var r);
            var x = Native.Consume(status, r);
            using var j = x.Json!;
            var v = j.RootElement;
            return new(v.GetProperty("id").GetUInt16(), v.GetProperty("name").GetString()!, v.GetProperty("encoding").GetString()!, x.Data);
        }
    }

    /// <summary>Copies a channel and its schema already encountered by this session. Unknown IDs do not terminate the session.</summary>
    public McapChannel GetChannel(ushort id)
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_reader_describe(handle, Protocol.DeclarationKind.Channel, id, out var r);
            var x = Native.Consume(status, r);
            using var j = x.Json!;
            var v = j.RootElement;
            ushort schema = v.GetProperty("schemaId").GetUInt16();
            return new(id, v.GetProperty("topic").GetString()!, v.GetProperty("messageEncoding").GetString()!, schema == 0 ? null : GetSchema(schema), v.GetProperty("metadata").Deserialize<Dictionary<string, string>>()!);
        }
    }

    /// <summary>Returns an independent summary, or null when absent. A non-seekable input must reach EOF first. I/O or parsing failure terminates this session.</summary>
    public McapSummary? GetSummary()
    {
        lock (gate)
        {
            Check();
            if (!seekable && !ended)
                throw new NotSupportedException("Read the non-seekable stream to EOF first.");
            try
            {
                int status = Native.fm_reader_summary(handle, out var r);
                var x = Native.ConsumeReader(status, r, handle.Bridge);
                using var j = x.Json;
                return j?.RootElement.Deserialize<McapSummary>(JsonSupport.Options);
            }
            catch { failed = true; throw; }
        }
    }

    /// <summary>Copies the record at a byte offset from the MCAP origin (the initial Stream position for Stream inputs) without advancing the sequential cursor. Requires seekable input; operation failures terminate the session.</summary>
    public McapRawRecord ReadRecordAt(ulong offset)
    {
        lock (gate)
        {
            Check();
            if (!seekable)
                throw new NotSupportedException("Random access requires a seekable source.");
            try
            {
                int status = Native.fm_reader_record_at(handle, offset, out var r);
                var x = Native.ConsumeReader(status, r, handle.Bridge);
                return new(checked((byte)x.Value), x.Data);
            }
            catch { failed = true; throw; }
        }
    }

    /// <summary>Copies the indexed Chunk record and checks its opcode and total encoded length. The entire read and validation are serialized; any operation or index-validation failure terminates this session.</summary>
    public McapRawRecord ReadChunk(McapChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Random access requires a seekable source.");
            try
            {
                var record = ReadRecordAt(index.ChunkStartOffset);
                if (record.Opcode != 6 || (ulong)record.Data.Length + 9 != index.ChunkLength)
                    throw new McapException("Invalid chunk index.");
                return record;
            }
            catch { failed = true; throw; }
        }
    }

    /// <summary>Copies the indexed MessageIndex records in channel-ID order, preserving empty groups. Reads and validation form one serialized operation; opcode, channel or parsing failures terminate this session.</summary>
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Random access requires a seekable source.");
            try
            {
                var result = new List<McapMessageIndex>();
                foreach (var offset in index.MessageIndexOffsets.OrderBy(x => x.Key))
                {
                    var record = ReadRecordAt(offset.Value);
                    if (record.Opcode != 7)
                        throw new McapException("Invalid message index.");
                    var value = RecordDecoder.MessageIndex(record.Data);
                    if (value.ChannelId != offset.Key)
                        throw new McapException("Mismatched message index channel.");
                    result.Add(value);
                }
                return result;
            }
            catch { failed = true; throw; }
        }
    }

    bool ReadOwned(OwnedReadSink sink, byte wanted = 0)
    {
        lock (gate)
        {
            Check();
            sink.Reset();
            try
            {
                using var lease = sink.Acquire();
                int status = Native.fm_reader_owned(handle, wanted, sink.Sink, out var h, out var r);
                if (status < Protocol.Status.Success)
                {
                    var error = Native.ConsumeError(r);
                    sink.ThrowIfError();
                    handle.Bridge?.ThrowIfError();
                    throw error;
                }
                if (status != Protocol.Status.End) return true;
                ended = true;
                fullyValidated = strict && h.Reserved == 0 && !topLevel;
                ScannedRecordCount = r.Value;
                return false;
            }
            catch { failed = true; throw; }
        }
    }

    /// <summary>Returns independent mutable results, copying payloads and mutable declarations.
    /// Use caller-buffer, visitor or lease delivery when independent result objects are not needed.</summary>
    public IEnumerable<McapMessage> ReadMessages()
    {
        if (!messages) throw new InvalidOperationException("This is a record session.");
        var channels = new Dictionary<ushort, McapChannel>();
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.Message);
        while (ReadOwned(sink))
        {
            var h = sink.Header;
            if (!channels.TryGetValue(h.ChannelId, out var channel))
                channels.Add(h.ChannelId, channel = GetChannel(h.ChannelId));
            yield return new(OwnedReadSink.CopyChannel(channel), h.LogTime, h.PublishTime, h.Sequence, (byte[])sink.Value!);
        }
    }

    /// <summary>Advances this session and yields independently owned copies of raw record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords()
    {
        if (messages) throw new InvalidOperationException("This is a message session.");
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.Record);
        while (ReadOwned(sink)) yield return (McapRawRecord)sink.Value!;
    }

    /// <summary>Delivers the valid message prefix and returns the original structured McapException that ends parsing, if any. The failed session remains terminal. Callback exceptions propagate rather than becoming recovery results.</summary>
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        ArgumentNullException.ThrowIfNull(accept);
        ulong count = 0;
        using var iterator = ReadMessages().GetEnumerator();
        while (true)
        {
            McapMessage message;
            try
            {
                if (!iterator.MoveNext())
                    return new(count, IsFullyValidated, null);
                message = iterator.Current;
            }
            catch (McapException e)
            {
                return new(count, false, e);
            }

            accept(message);
            count++;
        }
    }

    /// <summary>Consumes the remaining strict expanded scan and returns its record count after full-file validation. Indexed sessions cannot establish full validation.</summary>
    public ulong ValidateRemaining()
    {
        if (!strict) throw new InvalidOperationException("Open with McapReaderOptions.Strict to validate the entire scan.");
        if (topLevel)
            throw new InvalidOperationException("Full validation requires expanded chunks.");
        byte[] buffer = new byte[65536];
        while (true)
        {
            var status = messages ? ReadNext(buffer, out _, out var length) : ReadNextRecord(buffer, out _, out length);
            if (status == McapReadStatus.EndOfStream)
                break;
            if (status == McapReadStatus.BufferTooSmall)
                buffer = new byte[checked((int)length)];
        }

        if (!IsFullyValidated)
            throw new InvalidOperationException("Indexed queries cannot validate the full source.");
        return ScannedRecordCount;
    }

    IEnumerable<T> ReadSelected<T>(byte wanted, OwnedReadSink.Kind kind)
    {
        if (messages) throw new InvalidOperationException("This is a message session.");
        using var sink = new OwnedReadSink(kind);
        while (ReadOwned(sink, wanted)) yield return (T)sink.Value!;
    }
    /// <summary>Consumes a record session and returns independent schemas, once per ID.</summary>
    public IEnumerable<McapSchema> ReadSchemas()
    {
        var seen = new HashSet<ushort>();
        foreach (var schema in ReadSelected<McapSchema>(3, OwnedReadSink.Kind.Schema))
            if (seen.Add(schema.Id)) yield return schema;
    }
    /// <summary>Consumes a record session and returns independent channels with schemas, once per ID.</summary>
    public IEnumerable<McapChannel> ReadChannels()
    {
        var seen = new HashSet<ushort>();
        foreach (var id in ReadSelected<ushort>(4, OwnedReadSink.Kind.ChannelId))
            if (seen.Add(id)) yield return GetChannel(id);
    }
    /// <summary>Consumes a record session and copies its metadata records.</summary>
    public IEnumerable<McapMetadata> ReadMetadata() => ReadSelected<McapMetadata>(12, OwnedReadSink.Kind.Metadata);
    /// <summary>Consumes a record session and copies its attachments, including independent payload arrays.</summary>
    public IEnumerable<McapAttachment> ReadAttachments() => ReadSelected<McapAttachment>(9, OwnedReadSink.Kind.Attachment);

    /// <summary>Releases the native reader and transfers its underlying Stream. Only Stream-backed sessions support transfer; release failure prevents reuse and leaves this session disposed.</summary>
    public Stream IntoInner()
    {
        lock (gate)
        {
            borrowed.CheckReentry(); handle.Bridge?.CheckReentry();
            ObjectDisposedException.ThrowIf(disposed, this);
            if (handle.Bridge is null) throw new NotSupportedException("Only Stream-backed sessions can transfer ownership.");
            disposed = true;
            return handle.Transfer();
        }
    }

    /// <summary>Releases parser state before its input and reports cleanup failures. Repeated disposal is a no-op.</summary>
    public void Dispose()
    {
        lock (gate)
        {
            borrowed.CheckReentry(); handle.Bridge?.CheckReentry();
            if (disposed)
                return;
            disposed = true;
            handle.Dispose();
        }
    }
}
