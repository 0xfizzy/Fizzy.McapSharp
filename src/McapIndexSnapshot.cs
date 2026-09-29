using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Owns a native copy of a seekable source and its official summary. Independent of the originating session.</summary>
public sealed class McapIndexSnapshot : IDisposable
{
    readonly SnapshotHandle handle;
    readonly object gate = new();
    internal McapIndexSnapshot(IntPtr p) => handle = new(p);
    public unsafe McapIndexSnapshot(ReadOnlySpan<byte> data)
    {
        Native.EnsureAvailable();
        fixed (byte* p = data) { var status = Native.fm_snapshot_bytes(p, (nuint)data.Length, out var h, out var r); Native.Consume(status, r).Json?.Dispose(); handle = new(h); }
    }
    public McapSummary? GetSummary()
    {
        lock (gate) { ObjectDisposedException.ThrowIf(handle.IsClosed, this); var status = Native.fm_snapshot_summary(handle, out var r); var response = Native.Consume(status, r); using var j = response.Json; return j?.RootElement.Deserialize<McapSummary>(JsonSupport.Options); }
    }
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapChunkIndex chunk)
    {
        lock (gate)
        {
            ReadMessageIndexes(chunk, [], out var n); var b = new byte[checked((int)n)]; ReadMessageIndexes(chunk, b, out _);
            var fields = new McapRecordFields(b); var groups = new Dictionary<ushort, List<McapMessageIndexEntry>>();
            while (!fields.IsEmpty) { var id = fields.ReadUInt16(); if (!groups.TryGetValue(id, out var list)) groups.Add(id, list = []); list.Add(new(fields.ReadUInt64(), fields.ReadUInt64())); }
            return groups.Select(g => new McapMessageIndex(g.Key, g.Value)).ToArray();
        }
    }
    public McapBufferReader OpenSummaryRecords() { lock (gate) { ObjectDisposedException.ThrowIf(handle.IsClosed, this); return Native.SummaryRecords(0, handle); } }
    public McapChannel GetChannel(ushort id)
    {
        lock (gate) { ObjectDisposedException.ThrowIf(handle.IsClosed, this); var status = Native.fm_snapshot_channel(handle, id, out var r); return McapBufferReader.DecodeChannel(Native.Consume(status, r)); }
    }
    public McapMessage SeekMessage(McapChunkIndex chunk, McapMessageIndexEntry entry)
    {
        lock (gate) { SeekMessage(chunk, entry, [], out _, out var n); var data = new byte[checked((int)n)]; SeekMessage(chunk, entry, data, out var h, out _); return new(GetChannel(h.ChannelId), h.LogTime, h.PublishTime, h.Sequence, data); }
    }
    public IEnumerable<McapMessage> ReadChunkMessages(McapChunkIndex chunk)
    {
        using var reader = OpenChunkReader(chunk);
        foreach (var message in reader.ReadMessages()) yield return message;
    }
    /// <summary>Opens an independent lazy cursor that remains valid after this snapshot is disposed.</summary>
    public unsafe McapBufferReader OpenChunkReader(McapChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            int size = IndexEncoding.Size(index);
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            try
            {
                new IndexEncoding(encoded).Write(index);
                fixed (byte* body = encoded)
                {
                    int status = Native.fm_snapshot_chunk_reader(handle, body, (nuint)size, out var p, out var r);
                    Native.Consume(status, r).Json?.Dispose();
                    return new(p);
                }
            }
            finally { NativeMemory.Free(allocated); }
        }
    }
    public void OpenChunkMessages(McapChunkIndex index) => Call(1, index, default, [], out _, out _);
    public McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length) => Call(7, null, default, destination, out header, out length);
    public McapReadStatus SeekMessage(McapChunkIndex chunk, McapMessageIndexEntry message, Span<byte> destination, out McapMessageHeader header, out ulong length) => Call(2, chunk, message, destination, out header, out length);
    public McapReadStatus ReadMetadata(McapMetadataIndex index, Span<byte> destination, out ulong length) => Call(3, index, default, destination, out _, out length);
    public McapReadStatus ReadAttachment(McapAttachmentIndex index, Span<byte> destination, out ulong length) => Call(4, index, default, destination, out _, out length);
    /// <summary>Copies packed entries: channel ID (u16), log time (u64), chunk-relative offset (u64), all little endian.</summary>
    public McapReadStatus ReadMessageIndexes(McapChunkIndex index, Span<byte> destination, out ulong length) => Call(5, index, default, destination, out _, out length);
    public ulong GetCompressedDataOffset(McapChunkIndex index) { Call(8, index, default, [], out _, out var value); return value; }
    public McapFooter ReadFooter()
    {
        Span<byte> b = stackalloc byte[20];
        Call(6, null, default, b, out _, out _);
        var v = McapRecordView.Parse(2, b);
        return v.Footer;
    }
    public McapMetadata ReadMetadata(McapMetadataIndex index) => RecordDecoder.Metadata(ReadOwned(3, index));
    public McapAttachment ReadAttachment(McapAttachmentIndex index) => RecordDecoder.Attachment(ReadOwned(4, index));
    byte[] ReadOwned(uint op, object index)
    {
        lock (gate) { Call(op, index, default, [], out _, out var n); var data = new byte[checked((int)n)]; Call(op, index, default, data, out _, out _); return data; }
    }
    unsafe McapReadStatus Call(uint op, object? index, McapMessageIndexEntry message, Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            if (op is 1 or 2 or 3 or 4 or 5 or 8) ArgumentNullException.ThrowIfNull(index);
            int size = IndexEncoding.Size(index);
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            try
            {
                new IndexEncoding(encoded).Write(index);
                fixed (byte* p = destination) fixed (byte* body = encoded)
                {
                    int status = Native.fm_snapshot_call(handle, op, body, (nuint)encoded.Length, message.LogTime, message.Offset, p, (nuint)destination.Length, out var h, out var r);
                    if (status < 0) throw Native.ConsumeError(r);
                    header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                    return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
                }
            }
            finally { NativeMemory.Free(allocated); }
        }
    }
    public void Dispose() { lock (gate) handle.Dispose(); }
}

public sealed partial class McapReadSession
{
    public McapIndexSnapshot OpenIndexSnapshot()
    {
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Snapshot requires a seekable source.");
            int status = Native.fm_snapshot_open(handle, out var p, out var r);
            try { Native.Consume(status, r).Json?.Dispose(); return new(p); }
            finally { handle.Bridge?.ThrowIfError(); }
        }
    }
    public unsafe McapReadStatus ReadRecordAt(ulong offset, Span<byte> destination, out byte opcode, out ulong length)
    {
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Random access requires a seekable source.");
            fixed (byte* p = destination)
            {
                int status = Native.fm_reader_record_into(handle, offset, p, (nuint)destination.Length, out opcode, out var r);
                if (status < 0) { var error = Native.ConsumeError(r); handle.Bridge?.ThrowIfError(); throw error; }
                length = r.Value;
                return status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
            }
        }
    }
}
internal sealed class SnapshotHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal SnapshotHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_snapshot_free(handle); return true; }
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_chunk_reader(SnapshotHandle h, byte* index, nuint length, out IntPtr p, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_bytes(byte* data, nuint n, out IntPtr p, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_footer(byte* data, nuint n, byte* dest, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_chunk_offset(ulong offset, byte* compression, nuint n, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_summary(SnapshotHandle h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_open(ReaderHandle h, out IntPtr p, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_call(SnapshotHandle h, uint op, byte* index, nuint indexLength, ulong messageTime, ulong messageOffset, byte* dest, nuint capacity, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_snapshot_free(IntPtr p);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_parse_record(byte op, byte* p, nuint n, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_record_into(ReaderHandle h, ulong offset, byte* p, nuint n, out byte opcode, out Result r);
}
