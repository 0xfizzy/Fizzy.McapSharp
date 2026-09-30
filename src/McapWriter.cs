using System.Text.Json;

namespace Fizzy.McapSharp;
public sealed partial class McapWriter : IDisposable
{
    readonly WriterHandle handle;
    readonly object gate = new();
    bool completed, failed, disposed;
    bool attachment;
    public McapWriter(string path, McapWriterOptions? options = null) : this(path, null, options, false)
    {
    }

    public McapWriter(Stream stream, McapWriterOptions? options = null, bool leaveOpen = false) : this(null, stream ?? throw new ArgumentNullException(nameof(stream)), options, leaveOpen)
    {
    }

    unsafe McapWriter(string? path, Stream? stream, McapWriterOptions? options, bool leaveOpen)
    {
        Native.EnsureAvailable();
        options ??= new();
        if (!Enum.IsDefined(options.Compression) || ((int)options.RecoverableErrors & ~31) != 0)
            throw new ArgumentOutOfRangeException(nameof(options));
        if (stream is null)
            ArgumentException.ThrowIfNullOrWhiteSpace(path);
        var config = JsonSerializer.SerializeToElement(options, new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase });
        var dict = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(config)!;
        dict["compression"] = JsonSerializer.SerializeToElement(options.Compression.ToString().ToLowerInvariant());
        var req = Native.Request(new { path = path is null ? null : Path.GetFullPath(path), options = dict });
        StreamBridge? bridge = stream is null ? null : new(stream, true, leaveOpen);
        try
        {
            var cb = bridge?.Callbacks ?? default;
            var status = Native.fm_writer_open(req, (nuint)req.Length, bridge is null ? null : &cb, out var p, out var r);
            try
            {
                Native.Consume(status, r).Json?.Dispose();
            }
            finally
            {
                bridge?.ThrowIfError();
            }

            GC.KeepAlive(options);
            handle = new(p, bridge);
        }
        catch
        {
            bridge?.Release();
            throw;
        }
    }

    void CheckResult(int status, Native.Result result, ref bool safeRejection)
    {
        if (status >= 0) { handle.Bridge?.ThrowIfError(); return; }
        var error = Native.ConsumeError(result, status == -2);
        handle.Bridge?.ThrowIfError();
        safeRejection = status == -2;
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

    public ushort RegisterSchema(string name, string encoding, ReadOnlySpan<byte> data) => checked((ushort)Call(1, new { name, encoding }, data));
    public ushort RegisterSchema(ushort id, string name, string encoding, ReadOnlySpan<byte> data) => checked((ushort)Call(1, new { id, name, encoding }, data));
    public ushort RegisterChannel(string topic, string messageEncoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null) => checked((ushort)Call(2, new { topic, encoding = messageEncoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>() }));
    public ushort RegisterChannel(ushort id, string topic, string messageEncoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null) => checked((ushort)Call(2, new { id, topic, encoding = messageEncoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>() }));
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

    public void WriteMetadata(string name, IReadOnlyDictionary<string, string> metadata) => Call(4, new { name, metadata });
    public void WriteAttachment(string name, string mediaType, ulong logTime, ulong createTime, ReadOnlySpan<byte> data) => Call(5, new { name, media_type = mediaType, log_time = logTime, create_time = createTime }, data);
    public void StartAttachment(string name, string mediaType, ulong logTime, ulong createTime, ulong length)
    {
        lock (gate)
        {
            Call(8, new { name, media_type = mediaType, log_time = logTime, create_time = createTime, length });
            attachment = true;
        }
    }

    public void WriteAttachmentBytes(ReadOnlySpan<byte> data)
    {
        lock (gate)
        {
            if (!attachment)
                throw new InvalidOperationException("No attachment in progress.");
            Call(9, null, data);
        }
    }

    public void FinishAttachment()
    {
        lock (gate)
        {
            if (!attachment)
                throw new InvalidOperationException("No attachment in progress.");
            Call(10, null);
            attachment = false;
        }
    }

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

    public void Flush() => Call(6, null);
    public void Complete()
    {
        lock (gate)
        {
            CheckAvailable();
            if (completed)
                return;
            Call(7, null);
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
                    var status = Native.fm_writer_call(handle, 13, [], 0, null, 0, out var result);
                    Native.Consume(status, result).Json?.Dispose();
                }
            }
            catch { failed = true; throw; }
        }
    }

    public unsafe McapSummary GetSummary()
    {
        lock (gate)
        {
            CheckCompleted();
            var status = Native.fm_writer_call(handle, 12, [], 0, null, 0, out var r);
            var response = Native.Consume(status, r);
            using var json = response.Json!;
            return json.RootElement.Deserialize<McapSummary>(JsonSupport.Options)!;
        }
    }

    unsafe ulong Call(uint op, object? args, ReadOnlySpan<byte> data = default)
    {
        lock (gate)
        {
            Check(op is 9 or 10);
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

    public Stream IntoInner()
    {
        lock (gate)
        {
            handle.Bridge?.CheckReentry();
            ObjectDisposedException.ThrowIf(disposed, this);
            var stream = handle.Bridge?.Detach() ?? throw new NotSupportedException("Only Stream-backed sessions can transfer ownership.");
            Dispose();
            return stream;
        }
    }

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

internal static class JsonSupport
{
    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true
    };
}
