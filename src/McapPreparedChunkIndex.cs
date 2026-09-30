using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>An immutable, native-owned copy of a Chunk index for repeated operations across snapshots.</summary>
public sealed class McapPreparedChunkIndex : IDisposable
{
    internal readonly PreparedChunkIndexHandle Handle;
    internal readonly object Gate = new();
    public McapPreparedChunkIndex(McapChunkIndex index) : this(index,null) { }
    public unsafe McapPreparedChunkIndex(McapChunkIndex index, McapMemoryBudget? budget)
    {
        ArgumentNullException.ThrowIfNull(index);
        Native.EnsureAvailable();
        // Freeze the caller's mutable dictionary before sizing/encoding.
        var owned = index with { MessageIndexOffsets = new Dictionary<ushort, ulong>(index.MessageIndexOffsets) };
        var encoded = new byte[IndexEncoding.Size(owned)];
        new IndexEncoding(encoded).Write(owned);
        fixed (byte* p = encoded)
        {
            int status = Native.fm_chunk_index_prepare_budget(p, (nuint)encoded.Length, budget?.Id ?? 0, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();
            Handle = new(h);
            GC.KeepAlive(budget);
        }
    }
    internal void Check() => ObjectDisposedException.ThrowIf(Handle.IsClosed, this);
    public void Dispose() { lock (Gate) Handle.Dispose(); }
}
internal sealed class PreparedChunkIndexHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal PreparedChunkIndexHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_chunk_index_free(handle); NativeStorageSignal.Pulse(); return true; }
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_chunk_index_prepare_budget(byte* data, nuint n, ulong budgetId, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_chunk_index_prepare(byte* data, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_chunk_index_free(IntPtr h);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_prepared_call(SnapshotHandle h, uint op, PreparedChunkIndexHandle index, ulong time, ulong offset, byte* dest, nuint n, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_prepared_chunk_reader(SnapshotHandle h, PreparedChunkIndexHandle index, out IntPtr reader, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_message_owned(SnapshotHandle h, byte* data, nuint n, IntPtr prepared, ulong time, ulong offset, OwnedSink sink, out Result r);
}

public sealed partial class McapIndexSnapshot
{
    public McapBufferReader OpenChunkReader(McapPreparedChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (index.Gate)
        {
            Check(); index.Check();
            int status = Native.fm_snapshot_prepared_chunk_reader(handle, index.Handle, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();
            return new(h);
        }
    }
    public McapReadStatus SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry, Span<byte> destination, out McapMessageHeader header, out ulong length) => CallPrepared(2, index, entry, destination, out header, out length);
    public McapReadStatus ReadMessageIndexes(McapPreparedChunkIndex index, Span<byte> destination, out ulong length) => CallPrepared(5, index, default, destination, out _, out length);
    public ulong GetCompressedDataOffset(McapPreparedChunkIndex index) { CallPrepared(8, index, default, [], out _, out var n); return n; }
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapPreparedChunkIndex index)
    {
        lock (gate)
        {
            ReadMessageIndexes(index, [], out var n); var b = new byte[checked((int)n)]; ReadMessageIndexes(index, b, out _);
            return DecodeMessageIndexes(b);
        }
    }
    public McapMessage SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry) => SeekOwned(null, index, entry);
    public IEnumerable<McapMessage> ReadChunkMessages(McapPreparedChunkIndex index)
    {
        using var reader = OpenChunkReader(index);
        foreach (var message in reader.ReadMessages()) yield return message;
    }
    unsafe McapReadStatus CallPrepared(uint op, McapPreparedChunkIndex index, McapMessageIndexEntry entry, Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (index.Gate)
        {
            Check(); index.Check();
            fixed (byte* p = destination)
            {
                int status = Native.fm_snapshot_prepared_call(handle, op, index.Handle, entry.LogTime, entry.Offset, p, (nuint)destination.Length, out var h, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Message;
            }
        }
    }
    unsafe McapMessage SeekOwned(McapChunkIndex? index, McapPreparedChunkIndex? prepared, McapMessageIndexEntry entry)
    {
        if (prepared is null) ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (prepared?.Gate ?? gate)
        {
            Check(); prepared?.Check();
            using var sink = new OwnedReadSink(OwnedReadSink.Kind.Message);
            int size = prepared is null ? IndexEncoding.Size(index) : 0;
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            bool added = false;
            try
            {
                if (prepared is null) new IndexEncoding(encoded).Write(index);
                // The second native handle is optional, so hold its SafeHandle explicitly.
                prepared?.Handle.DangerousAddRef(ref added);
                fixed (byte* p = encoded)
                {
                    using var lease = sink.Acquire();
                    int status = Native.fm_snapshot_message_owned(handle, p, (nuint)size, prepared?.Handle.DangerousGetHandle() ?? IntPtr.Zero, entry.LogTime, entry.Offset, sink.Sink, out var r);
                    if (status < 0) { var error = Native.ConsumeError(r); sink.ThrowIfError(); throw error; }
                }
                var h = sink.Header;
                return new(GetChannel(h.ChannelId), h.LogTime, h.PublishTime, h.Sequence, (byte[])sink.Value!);
            }
            finally { if (added) prepared!.Handle.DangerousRelease(); NativeMemory.Free(allocated); }
        }
    }
}
