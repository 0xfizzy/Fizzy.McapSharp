using System.Text.Json;

namespace Fizzy.McapSharp;
/// <summary>Owns one native reader. Calls are serialized and cannot be reentered from Stream callbacks.</summary>
public sealed class McapReadSession : IDisposable
{
    readonly ReaderHandle handle;
    readonly bool seekable;
    readonly object gate = new();
    bool disposed, failed, ended, fullyValidated;
    readonly bool messages;
    readonly bool topLevel;
    public bool IsComplete => ended && !failed && fullyValidated;
    public ulong ScannedRecordCount { get; private set; }

    internal unsafe McapReadSession(string? path, Stream? stream, McapQuery? query, bool messages, McapRecordMode mode, bool leaveOpen)
    {
        if (!Enum.IsDefined(mode))
            throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        if (query?.StartTime > query?.EndTime)
            throw new ArgumentException("StartTime must not exceed EndTime.", nameof(query));
        this.messages = messages;
        topLevel = mode == McapRecordMode.TopLevel;
        seekable = stream?.CanSeek ?? true;
        var request = Native.Request(new { path, messages, topLevel = mode == McapRecordMode.TopLevel, topic = query?.Topic, start = query?.StartTime, end = query?.EndTime });
        StreamBridge? bridge = stream is null ? null : new(stream, false, leaveOpen);
        try
        {
            var cb = bridge?.Callbacks ?? default;
            int status = Native.fm_reader_open(request, (nuint)request.Length, bridge is null ? null : &cb, out var p, out var r);
            try
            {
                Native.Consume(status, r).Json?.Dispose();
            }
            finally
            {
                bridge?.ThrowIfError();
            }

            handle = new(p, bridge);
        }
        catch
        {
            bridge?.Release();
            throw;
        }
    }

    void Check()
    {
        handle.Bridge?.CheckReentry();
        ObjectDisposedException.ThrowIf(disposed, this);
        if (failed)
            throw new InvalidOperationException("Reader failed; open a new session.");
    }

    public McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong requiredLength)
    {
        if (!messages)
            throw new InvalidOperationException("This is a record session.");
        return Read(destination, out header, out _, out requiredLength);
    }

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
                    if (status < 0)
                    {
                        var error = Native.ConsumeError(r);
                        handle.Bridge?.ThrowIfError();
                        throw new McapException(error);
                    }

                    if (status == 1)
                    {
                        ended = true;
                        fullyValidated = h.Reserved == 0 && !topLevel;
                        ScannedRecordCount = r.Value;
                        requiredLength = 0;
                        header = default;
                        opcode = 0;
                        return McapReadStatus.EndOfStream;
                    }

                    return status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
                }
            }
            catch
            {
                failed = true;
                throw;
            }
        }
    }

    public McapSchema GetSchema(ushort id)
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_reader_describe(handle, 1, id, out var r);
            var x = Native.Consume(status, r);
            using var j = x.Json!;
            var v = j.RootElement;
            return new(v.GetProperty("id").GetUInt16(), v.GetProperty("name").GetString()!, v.GetProperty("encoding").GetString()!, x.Data);
        }
    }

    public McapChannel GetChannel(ushort id)
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_reader_describe(handle, 2, id, out var r);
            var x = Native.Consume(status, r);
            using var j = x.Json!;
            var v = j.RootElement;
            ushort schema = v.GetProperty("schemaId").GetUInt16();
            return new(id, v.GetProperty("topic").GetString()!, v.GetProperty("messageEncoding").GetString()!, schema == 0 ? null : GetSchema(schema), v.GetProperty("metadata").Deserialize<Dictionary<string, string>>()!);
        }
    }

    public McapSummary? GetSummary()
    {
        lock (gate)
        {
            Check();
            if (!seekable && !ended)
                throw new NotSupportedException("Read the non-seekable stream to EOF first.");
            int status = Native.fm_reader_summary(handle, out var r);
            try
            {
                var x = Native.Consume(status, r);
                using var j = x.Json;
                return j?.RootElement.Deserialize<McapSummary>(JsonSupport.Options);
            }
            finally
            {
                handle.Bridge?.ThrowIfError();
            }
        }
    }

    public McapRecord ReadRecordAt(ulong offset)
    {
        lock (gate)
        {
            Check();
            if (!seekable)
                throw new NotSupportedException("Random access requires a seekable source.");
            int status = Native.fm_reader_record_at(handle, offset, out var r);
            try
            {
                var x = Native.Consume(status, r);
                return new(checked((byte)x.Value), x.Data);
            }
            finally
            {
                handle.Bridge?.ThrowIfError();
            }
        }
    }

    public McapRecord ReadChunk(McapChunkIndex index)
    {
        var record = ReadRecordAt(index.ChunkStartOffset);
        if (record.Opcode != 6 || (ulong)record.Data.Length + 9 != index.ChunkLength)
            throw new McapException("Invalid chunk index.");
        return record;
    }

    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapChunkIndex index)
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

    public IEnumerable<McapMessage> ReadMessages()
    {
        if (!messages)
            throw new InvalidOperationException("This is a record session.");
        var channels = new Dictionary<ushort, McapChannel>();
        byte[] buffer = [];
        while (true)
        {
            var status = ReadNext(buffer, out var h, out var required);
            if (status == McapReadStatus.EndOfStream)
                yield break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)required)];
                continue;
            }

            if (!channels.TryGetValue(h.ChannelId, out var channel))
            {
                channel = GetChannel(h.ChannelId);
                channels.Add(h.ChannelId, channel);
            }

            yield return new(channel, h.LogTime, h.PublishTime, h.Sequence, buffer.AsSpan(0, checked((int)required)).ToArray());
        }
    }

    public IEnumerable<McapRecord> ReadRecords()
    {
        if (messages)
            throw new InvalidOperationException("This is a message session.");
        byte[] buffer = [];
        while (true)
        {
            var status = ReadNextRecord(buffer, out var opcode, out var length);
            if (status == McapReadStatus.EndOfStream)
                yield break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)length)];
                continue;
            }

            yield return new(opcode, buffer.AsSpan(0, checked((int)length)).ToArray());
        }
    }

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
                    return new(count, IsComplete, null);
                message = iterator.Current;
            }
            catch (McapException e)
            {
                return new(count, false, e.Message);
            }

            accept(message);
            count++;
        }
    }

    public ulong ValidateRemaining()
    {
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

        if (!IsComplete)
            throw new InvalidOperationException("Indexed queries cannot validate the full source.");
        return ScannedRecordCount;
    }

    public IEnumerable<McapSchema> ReadSchemas()
    {
        var seen = new HashSet<ushort>();
        foreach (var r in ReadRecords())
            if (r.Opcode == 3)
            {
                var s = RecordDecoder.Schema(r.Data);
                if (seen.Add(s.Id))
                    yield return s;
            }
    }

    public IEnumerable<McapChannel> ReadChannels()
    {
        var seen = new HashSet<ushort>();
        foreach (var r in ReadRecords())
            if (r.Opcode == 4)
            {
                var id = RecordDecoder.ChannelId(r.Data);
                if (seen.Add(id))
                    yield return GetChannel(id);
            }
    }

    public IEnumerable<McapMetadata> ReadMetadata()
    {
        foreach (var r in ReadRecords())
            if (r.Opcode == 12)
                yield return RecordDecoder.Metadata(r.Data);
    }

    public IEnumerable<McapAttachment> ReadAttachments()
    {
        foreach (var r in ReadRecords())
            if (r.Opcode == 9)
                yield return RecordDecoder.Attachment(r.Data);
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
