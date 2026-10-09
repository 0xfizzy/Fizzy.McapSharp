using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapIndexSnapshot
{
    /// <summary>Within this call, loads each distinct chunk once and returns a shared batch in request order. Cross-call reuse depends on the snapshot cache; returned leases may retain whole chunks or input storage.</summary>
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
                    if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
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
    /// <summary>Synchronously visits the indexed message. The payload expires when the callback returns; callback re-entry into this snapshot is forbidden.</summary>
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
                if (status < Protocol.Status.Success) { var error = Native.ConsumeError(result); borrowed.ThrowIfError(); throw error; }
            }
            finally { borrowed.Release(); if (added) index.Handle.DangerousRelease(); }
        }
    }
    /// <summary>Returns cumulative chunk-cache hits and loads for this snapshot. Counts do not measure bytes, resident memory or full-file validation.</summary>
    public McapCacheStatistics GetCacheStatistics()
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_snapshot_cache_statistics(handle, out var hits, out var loads, out var result);
            if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
            return new(hits, loads);
        }
    }
}
