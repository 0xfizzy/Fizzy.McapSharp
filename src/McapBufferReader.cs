using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public enum McapBufferReadMode { Linear, SansMagic, FlattenChunks, Chunk, RawMessages, Messages }

/// <summary>Lazy adapters for official slice-reader semantics. Construction copies input; advancement parses records.</summary>
public sealed class McapBufferReader : IDisposable
{
    readonly BufferReaderHandle handle;
    readonly object gate = new();
    internal McapBufferReader(IntPtr p) => handle = new(p);
    public McapBufferReader(ReadOnlySpan<byte> data, McapBufferReadMode mode = McapBufferReadMode.Messages, bool ignoreEndMagic = false)
        : this(data, mode, ignoreEndMagic, null) { }
    public unsafe McapBufferReader(ReadOnlySpan<byte> data, McapBufferReadMode mode, bool ignoreEndMagic, McapMemoryOptions? options)
    {
        if (!Enum.IsDefined(mode)) throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        var config = Native.Request(options ?? new());
        fixed (byte* p = data)
        {
            var status = Native.fm_buffer_reader_open_options((uint)mode, ignoreEndMagic, p, (nuint)data.Length, config, (nuint)config.Length, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose(); handle = new(h);
        }
    }
    /// <summary>Maps immutable file contents until this reader is disposed.</summary>
    public static McapBufferReader OpenMapped(string path, McapBufferReadMode mode = McapBufferReadMode.Messages,
        bool ignoreEndMagic = false, McapMemoryOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        if (!Enum.IsDefined(mode)) throw new ArgumentOutOfRangeException(nameof(mode));
        Native.EnsureAvailable();
        var config = Native.Request(new { path = Path.GetFullPath(path), mode = (uint)mode, ignoreEndMagic, options });
        int status = Native.fm_buffer_reader_mapped(config, (nuint)config.Length, out var p, out var r);
        Native.Consume(status, r).Json?.Dispose();
        return new(p);
    }
    public unsafe McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            fixed (byte* p = destination)
            {
                var status = Native.fm_buffer_reader_next(handle, p, (nuint)destination.Length, out opcode, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                length = r.Value;
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
            }
        }
    }
    public unsafe McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            fixed (byte* p = destination)
            {
                int status = Native.fm_buffer_reader_message(handle, p, (nuint)destination.Length, out var h, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
            }
        }
    }
    public McapChannel GetChannel(ushort id)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
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
    public IEnumerable<McapRecord> ReadRecords()
    {
        byte[] b = [];
        while (true)
        {
            var status = ReadNextRecord(b, out var opcode, out var n);
            if (status == McapReadStatus.EndOfStream) yield break;
            if (status == McapReadStatus.BufferTooSmall) { b = new byte[checked((int)n)]; continue; }
            yield return new(opcode, b.AsSpan(0, checked((int)n)).ToArray());
        }
    }
    public IEnumerable<McapMessage> ReadMessages()
    {
        var channels = new Dictionary<ushort, McapChannel>();
        byte[] buffer = [];
        while (true)
        {
            var status = ReadNextRecord(buffer, out var opcode, out var length);
            if (status == McapReadStatus.EndOfStream) yield break;
            if (status == McapReadStatus.BufferTooSmall) { buffer = new byte[checked((int)length)]; continue; }
            if (opcode != 5) continue;
            var h = DecodeMessageHeader(buffer);
            if (!channels.TryGetValue(h.ChannelId, out var channel))
                channels.Add(h.ChannelId, channel = GetChannel(h.ChannelId));
            // Keep the cached description private: public arrays and dictionaries are mutable.
            var schema = channel.Schema;
            var ownedChannel = channel with
            {
                Schema = schema is null ? null : schema with { Data = schema.Data.AsSpan().ToArray() },
                Metadata = new Dictionary<string, string>(channel.Metadata)
            };
            yield return new(ownedChannel, h.LogTime, h.PublishTime, h.Sequence,
                buffer.AsSpan(22, checked((int)length) - 22).ToArray());
        }
    }
    static McapMessageHeader DecodeMessageHeader(byte[] body)
    {
        // ReadNextRecord already validated the body with the official parser.
        var fields = new McapRecordFields(body);
        return new(fields.ReadUInt16(), fields.ReadUInt32(), fields.ReadUInt64(), fields.ReadUInt64());
    }
    public McapMemoryStatistics GetMemoryStatistics() { lock (gate) { ObjectDisposedException.ThrowIf(handle.IsClosed, this); return Native.MemoryStatistics(1, handle); } }
    public void Dispose() { lock (gate) handle.Dispose(); }
}
internal sealed class BufferReaderHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal BufferReaderHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_buffer_reader_free(handle); return true; }
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
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_buffer_reader_free(IntPtr h);
}
