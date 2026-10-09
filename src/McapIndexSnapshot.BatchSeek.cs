using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapIndexSnapshot
{
    /// <summary>Within this call, loads each distinct chunk once and returns a shared batch in request order. Cross-call reuse depends on the snapshot cache; returned leases may retain whole chunks or input storage.</summary>
    public unsafe McapMessageBatchLease SeekMessages(ReadOnlySpan<McapSeekRequest> requests)
    {
        Native.CheckLeaseRequest(requests.Length, 1);
        McapPreparedChunkIndex.CheckCallbackReentry();
        // Acquire distinct descriptors in one order before the snapshot, as all prepared calls do.
        // Setup allocation is permitted for batch leases; no payload is copied here.
        var indexes = new McapPreparedChunkIndex[requests.Length];
        for (int i = 0; i < requests.Length; i++)
        {
            ArgumentNullException.ThrowIfNull(requests[i].Index);
            indexes[i] = requests[i].Index;
        }
        Array.Sort(indexes, static (a, b) => a.LockOrder.CompareTo(b.LockOrder));
        int unique = 0;
        foreach (var index in indexes)
            if (unique == 0 || !ReferenceEquals(indexes[unique - 1], index)) indexes[unique++] = index;
        Native.SeekRequest[]? rented = null;
        Span<Native.SeekRequest> native = requests.Length <= 256 ? stackalloc Native.SeekRequest[requests.Length]
            : (rented = ArrayPool<Native.SeekRequest>.Shared.Rent(requests.Length)).AsSpan(0, requests.Length);
        int locked = 0;
        try
        {
            for (; locked < unique; locked++) Monitor.Enter(indexes[locked].Gate);
            lock (gate)
            {
                Check();
                for (int i = 0; i < requests.Length; i++)
                {
                    var index = requests[i].Index;
                    index.Check();
                    native[i] = new() { Index = index.Handle.DangerousGetHandle(), Time = requests[i].Entry.LogTime, Offset = requests[i].Entry.Offset };
                }
                fixed (Native.SeekRequest* p = native)
                {
                    int status = Native.fm_snapshot_seek_batch(handle, p, (nuint)requests.Length, out var batch, out var result);
                    if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
                    return new(batch, requests.Length);
                }
            }
        }
        finally
        {
            while (locked > 0) Monitor.Exit(indexes[--locked].Gate);
            if (rented is not null) ArrayPool<Native.SeekRequest>.Shared.Return(rented);
            GC.KeepAlive(indexes);
        }
    }
    /// <summary>Synchronously visits the indexed message. The payload expires when the callback returns; callback re-entry into this snapshot, any prepared-index operation, or prepared-index disposal is forbidden.</summary>
    public unsafe void SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry, McapMessageVisitor visitor)
    {
        ArgumentNullException.ThrowIfNull(index);
        ArgumentNullException.ThrowIfNull(visitor);
        McapPreparedChunkIndex.CheckCallbackReentry();
        lock (index.Gate) lock (gate)
        {
            Check(); index.Check(); bool added = false;
            try
            {
                index.Handle.DangerousAddRef(ref added);
                McapPreparedChunkIndex.EnterCallback();
                int status = Native.fm_snapshot_message_owned(handle, null, 0, index.Handle.DangerousGetHandle(), entry.LogTime, entry.Offset, borrowed.Acquire(visitor, true), out var result);
                if (status < Protocol.Status.Success) { var error = Native.ConsumeError(result); borrowed.ThrowIfError(); throw error; }
            }
            finally { McapPreparedChunkIndex.ExitCallback(); borrowed.Release(); if (added) index.Handle.DangerousRelease(); }
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
