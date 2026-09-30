using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public readonly record struct McapSeekRequest(McapPreparedChunkIndex Index, McapMessageIndexEntry Entry);
public readonly record struct McapCacheStatistics(ulong Hits, ulong ChunkLoads);

public sealed partial class McapIndexSnapshot
{
    readonly BorrowedReadSink borrowed = new();
    void Check() { borrowed.CheckReentry(); ObjectDisposedException.ThrowIf(handle.IsClosed, this); }
    /// <summary>Loads each distinct chunk once and returns messages in request order.</summary>
    public unsafe McapMessageBatchLease SeekMessages(ReadOnlySpan<McapSeekRequest> requests)
    {
        Native.CheckLeaseRequest(requests.Length, 1);
        Native.SeekRequest[]? rented = null;
        Span<Native.SeekRequest> native = requests.Length <= 256 ? stackalloc Native.SeekRequest[requests.Length]
            : (rented = ArrayPool<Native.SeekRequest>.Shared.Rent(requests.Length)).AsSpan(0, requests.Length);
        int retained = 0;
        lock (gate)
        {
            try
            {
                Check();
                for (int i = 0; i < requests.Length; i++)
                {
                    var index = requests[i].Index;
                    ArgumentNullException.ThrowIfNull(index);
                    bool added = false;
                    index.Handle.DangerousAddRef(ref added);
                    retained++;
                    native[i] = new() { Index = index.Handle.DangerousGetHandle(), Time = requests[i].Entry.LogTime, Offset = requests[i].Entry.Offset };
                }
                fixed (Native.SeekRequest* p = native)
                {
                    int status = Native.fm_snapshot_seek_batch(handle, p, (nuint)requests.Length, out var batch, out var result);
                    if (status < 0) throw Native.ConsumeError(result);
                    return new(batch, requests.Length);
                }
            }
            finally
            {
                for (int i = 0; i < retained; i++) requests[i].Index.Handle.DangerousRelease();
                if (rented is not null) ArrayPool<Native.SeekRequest>.Shared.Return(rented);
            }
        }
    }
    public unsafe void SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry, McapMessageVisitor visitor)
    {
        ArgumentNullException.ThrowIfNull(index);
        ArgumentNullException.ThrowIfNull(visitor);
        lock (gate) lock (index.Gate)
        {
            Check(); index.Check(); bool added = false;
            try
            {
                index.Handle.DangerousAddRef(ref added);
                int status = Native.fm_snapshot_message_owned(handle, null, 0, index.Handle.DangerousGetHandle(), entry.LogTime, entry.Offset, borrowed.Acquire(visitor, true), out var result);
                if (status < 0) { var error = Native.ConsumeError(result); borrowed.ThrowIfError(); throw error; }
            }
            finally { borrowed.Release(); if (added) index.Handle.DangerousRelease(); }
        }
    }
    public McapCacheStatistics GetCacheStatistics()
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_snapshot_cache_statistics(handle, out var hits, out var loads, out var result);
            if (status < 0) throw Native.ConsumeError(result);
            return new(hits, loads);
        }
    }
}
internal static partial class Native
{
    [StructLayout(LayoutKind.Sequential)]
    internal struct SeekRequest { internal IntPtr Index; internal ulong Time, Offset; }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_seek_batch(SnapshotHandle snapshot, SeekRequest* requests, nuint count, out IntPtr batch, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_cache_statistics(SnapshotHandle snapshot, out ulong hits, out ulong loads, out Result result);
}
