namespace Fizzy.McapSharp;

/// <summary>A serialized, file-backed MCAP writer. Complete must succeed before the file is considered complete.</summary>
public sealed class McapWriter : IDisposable
{
    private readonly WriterHandle handle;
    private readonly object gate = new();
    private bool completed, failed, disposed;
    public McapWriter(string path, McapWriterOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        Native.EnsureAvailable();
        options ??= new();
        if (!Enum.IsDefined(options.Compression)) throw new ArgumentOutOfRangeException(nameof(options));
        if (options.ChunkSize == 0) throw new ArgumentOutOfRangeException(nameof(options));
        var request = Native.Request(new { path = Path.GetFullPath(path), compression = options.Compression.ToString().ToLowerInvariant(), chunk_size = options.ChunkSize, use_chunks = options.UseChunks, indexes = options.EmitIndexes, profile = options.Profile });
        var status = Native.fm_writer_open(request, (nuint)request.Length, out var pointer, out var result);
        Native.Consume(status, result).Json?.Dispose();
        handle = new(pointer);
    }
    public ushort RegisterSchema(string name, string encoding, ReadOnlySpan<byte> data)
        => checked((ushort)Call(1, new { name, encoding }, data));
    public ushort RegisterChannel(string topic, string messageEncoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null)
        => checked((ushort)Call(2, new { topic, encoding = messageEncoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string,string>() }));
    public void WriteMessage(ushort channelId, ulong logTime, ulong publishTime, uint sequence, ReadOnlySpan<byte> data)
        => Call(3, new { channel_id = channelId, log_time = logTime, publish_time = publishTime, sequence }, data);
    public void WriteMetadata(string name, IReadOnlyDictionary<string, string> metadata)
        => Call(4, new { name, metadata });
    public void WriteAttachment(string name, string mediaType, ulong logTime, ulong createTime, ReadOnlySpan<byte> data)
        => Call(5, new { name, media_type = mediaType, log_time = logTime, create_time = createTime }, data);
    public void Flush() => Call(6, new { });
    public void Complete()
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (completed) return;
            Call(7, new { }); completed = true;
        }
    }
    private ulong Call(uint operation, object args, ReadOnlySpan<byte> data = default)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (completed) throw new InvalidOperationException("Writer is complete.");
            if (failed) throw new InvalidOperationException("Writer failed; dispose it and start a new file.");
            var request = Native.Request(args); var payload = data.ToArray();
            try
            {
                var status = Native.fm_writer_call(handle, operation, request, (nuint)request.Length, payload, (nuint)payload.Length, out var result);
                var response = Native.Consume(status, result); response.Json?.Dispose(); return response.Value;
            }
            catch { failed = true; throw; }
        }
    }
    public void Dispose()
    {
        lock (gate) { if (disposed) return; disposed = true; handle.Dispose(); }
    }
}
