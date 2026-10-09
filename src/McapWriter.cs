using System.Text.Json;

namespace Fizzy.McapSharp;
/// <summary>A serialized MCAP writer. Complete explicitly to finish the format; disposal only releases resources. Native or I/O failures are terminal except for configured audited pre-mutation rejections.</summary>
public sealed partial class McapWriter : IDisposable
{
    readonly WriterHandle handle;
    readonly object gate = new();
    bool completed, failed, disposed;
    bool attachment;
    /// <summary>Creates a new file, failing if the path exists. The writer owns the file until disposal.</summary>
    public McapWriter(string path, McapWriterOptions? options = null) : this(path, null, options, false)
    {
    }

    /// <summary>Writes to the supplied stream at its current end. The caller selects file creation/truncation; leaveOpen preserves stream ownership on disposal.</summary>
    public McapWriter(Stream stream, McapWriterOptions? options = null, bool leaveOpen = false) : this(null, stream ?? throw new ArgumentNullException(nameof(stream)), options, leaveOpen)
    {
    }

    unsafe McapWriter(string? path, Stream? stream, McapWriterOptions? options, bool leaveOpen)
    {
        Native.EnsureAvailable();
        options ??= new();
        ArgumentNullException.ThrowIfNull(options.Profile);
        if (!Enum.IsDefined(options.Compression) || ((int)options.SafeRejections & ~31) != 0)
            throw new ArgumentOutOfRangeException(nameof(options));
        if (stream is null)
            ArgumentException.ThrowIfNullOrWhiteSpace(path);
        var config = JsonSerializer.SerializeToElement(options, new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase });
        var dict = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(config)!;
        dict["recoverableErrors"] = dict["safeRejections"];
        dict.Remove("safeRejections");
        dict["compression"] = JsonSerializer.SerializeToElement(options.Compression.ToString().ToLowerInvariant());
        var req = Native.Request(new { path = path is null ? null : Path.GetFullPath(path), options = dict });
        StreamBridge? bridge = stream is null ? null : new(stream, true, leaveOpen);
        WriterHandle? opened = null;
        try
        {
            var cb = bridge?.Callbacks ?? default;
            var status = Native.fm_writer_open(req, (nuint)req.Length, bridge is null ? null : &cb, out var p, out var r);
            if (p != IntPtr.Zero) opened = new(p, bridge);
            try
            {
                Native.Consume(status, r).Json?.Dispose();
            }
            finally
            {
                bridge?.ThrowIfError();
            }

            handle = opened!;
        }
        catch (Exception operation)
        {
            try { if (opened is not null) opened.Dispose(); else bridge?.Release(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }

    void CheckResult(int status, Native.Result result, ref bool safeRejection)
    {
        if (status >= Protocol.Status.Success) { handle.Bridge?.ThrowIfError(); return; }
        var error = Native.ConsumeError(result, status == Protocol.WriterStatus.SafeRejection);
        handle.Bridge?.ThrowIfError();
        safeRejection = status == Protocol.WriterStatus.SafeRejection;
        throw error;
    }

    void CheckAvailable()
    {
        handle.Bridge?.CheckReentry();
        ObjectDisposedException.ThrowIf(disposed, this);
        if (failed) throw new InvalidOperationException("Writer failed; start a new recording.");
    }

    void CheckCompleted()
    {
        CheckAvailable();
        if (!completed) throw new InvalidOperationException("Complete must succeed first.");
    }

    void Check(bool allowAttachment = false)
    {
        CheckAvailable();
        if (completed)
            throw new InvalidOperationException("Writer is complete.");
        if (attachment && !allowAttachment)
            throw new InvalidOperationException("Finish the attachment first.");
    }

    /// <summary>Registers or deduplicates schema content and returns its allocated ID. Required strings must be non-null; data is consumed before returning.</summary>
    public ushort RegisterSchema(string name, string encoding, ReadOnlySpan<byte> data)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(encoding);
        return checked((ushort)Call(Protocol.WriterOperation.Schema, new { name, encoding }, data));
    }
    /// <summary>Registers schema content with an explicit nonzero ID. Conflicting declarations follow the configured safe-rejection policy.</summary>
    public ushort RegisterSchema(ushort id, string name, string encoding, ReadOnlySpan<byte> data)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(encoding);
        return checked((ushort)Call(Protocol.WriterOperation.Schema, new { id, name, encoding }, data));
    }
    /// <summary>Registers a channel and returns its allocated ID. Schema ID zero means no schema; null metadata means empty metadata.</summary>
    public ushort RegisterChannel(string topic, string messageEncoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null)
    {
        ArgumentNullException.ThrowIfNull(topic);
        ArgumentNullException.ThrowIfNull(messageEncoding);
        if (metadata is not null) ArgumentValidation.ValidateMetadata(metadata);
        return checked((ushort)Call(Protocol.WriterOperation.Channel, new { topic, encoding = messageEncoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>() }));
    }
    /// <summary>Registers a channel with an explicit ID, including zero. Schema ID zero means no schema; null metadata means empty metadata.</summary>
    public ushort RegisterChannel(ushort id, string topic, string messageEncoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null)
    {
        ArgumentNullException.ThrowIfNull(topic);
        ArgumentNullException.ThrowIfNull(messageEncoding);
        if (metadata is not null) ArgumentValidation.ValidateMetadata(metadata);
        return checked((ushort)Call(Protocol.WriterOperation.Channel, new { id, topic, encoding = messageEncoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>() }));
    }
    /// <summary>Writes a message on an already registered channel. Payload is consumed synchronously; the warmed path allocates zero managed bytes.</summary>
    public unsafe void WriteMessage(in McapMessageHeader header, ReadOnlySpan<byte> data)
    {
        lock (gate)
        {
            Check();
            var h = new Native.NativeHeader
            {
                ChannelId = header.ChannelId,
                Sequence = header.Sequence,
                LogTime = header.LogTime,
                PublishTime = header.PublishTime
            };
            bool safeRejection = false;
            try
            {
                fixed (byte* p = data)
                {
                    var status = Native.fm_writer_message(handle, &h, p, (nuint)data.Length, out var r);
                    CheckResult(status, r, ref safeRejection);
                }
            }
            catch
            {
                if (!safeRejection) failed = true;
                throw;
            }
        }
    }

    /// <summary>Writes metadata. The name, dictionary, keys and values must be non-null; validation errors do not fail the writer.</summary>
    public void WriteMetadata(string name, IReadOnlyDictionary<string, string> metadata)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentValidation.ValidateMetadata(metadata);
        Call(Protocol.WriterOperation.Metadata, new { name, metadata });
    }
    /// <summary>Writes a complete attachment. Times use caller-defined nanoseconds; the payload is consumed synchronously.</summary>
    public void WriteAttachment(string name, string mediaType, ulong logTime, ulong createTime, ReadOnlySpan<byte> data)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(mediaType);
        Call(Protocol.WriterOperation.Attachment, new { name, media_type = mediaType, log_time = logTime, create_time = createTime }, data);
    }
    /// <summary>Starts a segmented attachment with an exact payload byte length. FinishAttachment is required before other writer operations.</summary>
    public void StartAttachment(string name, string mediaType, ulong logTime, ulong createTime, ulong length)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(mediaType);
        lock (gate)
        {
            Call(Protocol.WriterOperation.StartAttachment, new { name, media_type = mediaType, log_time = logTime, create_time = createTime, length });
            attachment = true;
        }
    }

    /// <summary>Consumes the next payload segment of the active attachment synchronously. Exceeding the declared size fails the writer.</summary>
    public void WriteAttachmentBytes(ReadOnlySpan<byte> data)
    {
        lock (gate)
        {
            if (!attachment)
                throw new InvalidOperationException("No attachment in progress.");
            Call(Protocol.WriterOperation.AttachmentBytes, null, data);
        }
    }

    /// <summary>Finishes the active attachment. A payload length mismatch is terminal.</summary>
    public void FinishAttachment()
    {
        lock (gate)
        {
            if (!attachment)
                throw new InvalidOperationException("No attachment in progress.");
            Call(Protocol.WriterOperation.FinishAttachment, null);
            attachment = false;
        }
    }

    /// <summary>Writes a private opcode (0x80-0xFF), optionally inside chunks. Payload is consumed synchronously.</summary>
    public unsafe void WritePrivateRecord(byte opcode, ReadOnlySpan<byte> data, bool includeInChunks = false)
    {
        if (opcode < 0x80) throw new ArgumentOutOfRangeException(nameof(opcode));
        lock (gate)
        {
            Check();
            bool safeRejection = false;
            try { fixed (byte* p = data) { var status = Native.fm_writer_private(handle, opcode, includeInChunks, p, (nuint)data.Length, out var r); CheckResult(status, r, ref safeRejection); } }
            catch { if (!safeRejection) failed = true; throw; }
        }
    }

    /// <summary>Flushes the current chunk and output buffers without completing the format or requesting durable persistence.</summary>
    public void Flush() => Call(Protocol.WriterOperation.Flush, null);
    /// <summary>Finishes the MCAP format and flushes output buffers. Repeated successful calls do nothing; disposal remains required.</summary>
    public void Complete()
    {
        lock (gate)
        {
            CheckAvailable();
            if (completed)
                return;
            Call(Protocol.WriterOperation.Complete, null);
            completed = true;
        }
    }

    /// <summary>Requests file persistence after successful completion. Supports path outputs and FileStream only.</summary>
    public unsafe void FlushToDisk()
    {
        lock (gate)
        {
            CheckCompleted();
            var bridge = handle.Bridge;
            if (bridge is not null && !bridge.CanFlushToDisk)
                throw new NotSupportedException("File persistence requires a path output or FileStream.");
            try
            {
                if (bridge is not null) bridge.FlushToDisk();
                else
                {
                    var status = Native.fm_writer_call(handle, Protocol.WriterOperation.FlushToDisk, [], 0, null, 0, out var result);
                    Native.Consume(status, result).Json?.Dispose();
                }
            }
            catch { failed = true; throw; }
        }
    }

    /// <summary>Returns an independent managed copy of the summary after successful Complete. Cost grows with summary contents.</summary>
    public unsafe McapSummary GetSummary()
    {
        lock (gate)
        {
            CheckCompleted();
            var status = Native.fm_writer_call(handle, Protocol.WriterOperation.Summary, [], 0, null, 0, out var r);
            var response = Native.Consume(status, r);
            using var json = response.Json!;
            return json.RootElement.Deserialize<McapSummary>(JsonSupport.Options)!;
        }
    }

    unsafe ulong Call(uint op, object? args, ReadOnlySpan<byte> data = default)
    {
        lock (gate)
        {
            Check(op is Protocol.WriterOperation.AttachmentBytes or Protocol.WriterOperation.FinishAttachment);
            var req = args is null ? [] : Native.Request(args);
            bool safeRejection = false;
            try
            {
                fixed (byte* payload = data)
                {
                    var status = Native.fm_writer_call(handle, op, req, (nuint)req.Length, payload, (nuint)data.Length, out var r);
                    CheckResult(status, r, ref safeRejection);
                    try
                    {
                        var response = Native.Consume(status, r);
                        response.Json?.Dispose();
                        return response.Value;
                    }
                    finally
                    {
                        handle.Bridge?.ThrowIfError();
                    }
                }
            }
            catch
            {
                if (!safeRejection) failed = true;
                throw;
            }
        }
    }

    /// <summary>Releases native writer state and transfers the underlying stream without implicit completion. Only stream-backed writers support transfer; cleanup errors are reported.</summary>
    public Stream IntoInner()
    {
        lock (gate)
        {
            handle.Bridge?.CheckReentry();
            ObjectDisposedException.ThrowIf(disposed, this);
            if (handle.Bridge is null) throw new NotSupportedException("Only Stream-backed sessions can transfer ownership.");
            disposed = true;
            return handle.Transfer();
        }
    }

    /// <summary>Releases native and owned stream resources without completing the format. Reports cleanup failures; repeated disposal does not replay release.</summary>
    public void Dispose()
    {
        lock (gate)
        {
            handle.Bridge?.CheckReentry();
            if (disposed)
                return;
            disposed = true;
            handle.Dispose();
        }
    }
}
