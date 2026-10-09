using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public enum McapBufferReadMode { Linear, SansMagic, FlattenChunks, Chunk, RawMessages, Messages }

/// <summary>Lazy adapters for official slice-reader semantics. Construction copies input; advancement parses records.</summary>
public sealed partial class McapBufferReader : IDisposable
{
    readonly BufferReaderHandle handle;
    readonly object gate = new();
    readonly BorrowedReadSink borrowed = new();
    void Check() { borrowed.CheckReentry(); ObjectDisposedException.ThrowIf(handle.IsClosed, this); }
    internal McapBufferReader(IntPtr p) => handle = new(p);
    public unsafe McapBufferReader(ReadOnlySpan<byte> data, McapBufferReadMode mode = McapBufferReadMode.Messages, bool ignoreEndMagic = false)
    {
        if (!Enum.IsDefined(mode)) throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        fixed (byte* p = data)
        {
            var status = Native.fm_buffer_reader_open((uint)mode, ignoreEndMagic, p, (nuint)data.Length, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();  handle = new(h);
        }
    }
    /// <summary>Maps file contents without copying. Keep the file unchanged until this reader and every lease retaining the mapping are disposed.</summary>
    public static McapBufferReader OpenMapped(string path, McapBufferReadMode mode = McapBufferReadMode.Messages,
        bool ignoreEndMagic = false)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        if (!Enum.IsDefined(mode)) throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        var config = Native.Request(new { path = Path.GetFullPath(path), mode = (uint)mode, ignoreEndMagic });
        int status = Native.fm_buffer_reader_mapped(config, (nuint)config.Length, out var p, out var r);
        Native.Consume(status, r).Json?.Dispose();

        return new(p);
    }
    /// <summary>Copies one raw record body into caller storage. BufferTooSmall retains the record and reports required capacity. The returned opcode identifies the body; length excludes the record header.</summary>
    public unsafe McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
    {
        lock (gate)
        {
            Check();
            fixed (byte* p = destination)
            {
                var status = Native.fm_buffer_reader_next(handle, p, (nuint)destination.Length, out opcode, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                length = r.Value;
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    /// <summary>Copies one message payload into caller storage. BufferTooSmall reports the required byte length and preserves the pending message for retry; EOF is not proof of complete validation.</summary>
    public unsafe McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        lock (gate)
        {
            Check();
            fixed (byte* p = destination)
            {
                int status = Native.fm_buffer_reader_message(handle, p, (nuint)destination.Length, out var h, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    public McapChannel GetChannel(ushort id)
    {
        lock (gate)
        {
            Check();
            var status = Native.fm_buffer_reader_channel(handle, id, out var r);
            return DecodeChannel(Native.Consume(status, r));
        }
    }
    internal static McapChannel DecodeChannel((JsonDocument? Json, byte[] Data, ulong Value) response)
    {
        using var j = response.Json!;
        var c = j.RootElement; var s = c.GetProperty("schema");
        McapSchema? schema = s.ValueKind == JsonValueKind.Null ? null : new(s.GetProperty("id").GetUInt16(), s.GetProperty("name").GetString()!, s.GetProperty("encoding").GetString()!, response.Data);
        return new(c.GetProperty("id").GetUInt16(), c.GetProperty("topic").GetString()!, c.GetProperty("messageEncoding").GetString()!, schema, c.GetProperty("metadata").Deserialize<Dictionary<string, string>>()!);
    }
    bool ReadOwned(OwnedReadSink sink)
    {
        lock (gate)
        {
            Check();
            sink.Reset();
            using var lease = sink.Acquire();
            int status = Native.fm_buffer_reader_owned(handle, false, sink.Sink, out var r);
            if (status < 0) { var error = Native.ConsumeError(r); sink.ThrowIfError(); throw error; }
            return status != 1;
        }
    }
    /// <summary>Advances this session and yields independently owned copies of raw record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords()
    {
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.Record);
        while (ReadOwned(sink)) yield return (McapRawRecord)sink.Value!;
    }
    public IEnumerable<McapMessage> ReadMessages()
    {
        var channels = new Dictionary<ushort, McapChannel>();
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.MessageBody);
        while (ReadOwned(sink))
        {
            if (sink.Value is not byte[] data) continue;
            var h = sink.Header;
            if (!channels.TryGetValue(h.ChannelId, out var channel))
                channels.Add(h.ChannelId, channel = GetChannel(h.ChannelId));
            yield return new(OwnedReadSink.CopyChannel(channel), h.LogTime, h.PublishTime, h.Sequence, data);
        }
    }
    public void Dispose() { lock (gate) { borrowed.CheckReentry(); handle.Dispose(); } }
}
internal sealed class BufferReaderHandle : OwnedNativeHandle
{
    internal BufferReaderHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_buffer_reader_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_mapped(byte[] config, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_open(uint mode, [MarshalAs(UnmanagedType.I1)] bool ignoreEnd, byte* p, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_next(BufferReaderHandle h, byte* p, nuint n, out byte opcode, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_message(BufferReaderHandle h, byte* p, nuint n, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_channel(BufferReaderHandle h, ushort id, out Result r);
}
