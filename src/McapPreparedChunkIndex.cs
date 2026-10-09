using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>An immutable, native-owned copy of a Chunk index for repeated operations across snapshots.</summary>
public sealed class McapPreparedChunkIndex : IDisposable
{
    internal readonly PreparedChunkIndexHandle Handle;
    internal readonly object Gate = new();
    static long nextLockOrder;
    internal readonly long LockOrder = Interlocked.Increment(ref nextLockOrder);
    [ThreadStatic] static bool inSnapshotCallback;
    internal static void CheckCallbackReentry()
    {
        if (inSnapshotCallback) throw new InvalidOperationException("Cannot use or dispose a prepared chunk index from a snapshot message callback.");
    }
    internal static void EnterCallback() => inSnapshotCallback = true;
    internal static void ExitCallback() => inSnapshotCallback = false;
    internal readonly ushort[] ChannelIds;
    /// <summary>Copies and freezes the complete descriptor, including the current channel-offset map, for repeated calls across snapshots. Does not validate any file.</summary>
    public unsafe McapPreparedChunkIndex(McapChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        Native.EnsureAvailable();
        // Freeze the caller's mutable dictionary before sizing/encoding.
        var owned = index with { MessageIndexOffsets = new Dictionary<ushort, ulong>(index.MessageIndexOffsets) };
        ChannelIds = owned.MessageIndexOffsets.Keys.ToArray();
        var encoded = new byte[IndexEncoding.Size(owned)];
        new IndexEncoding(encoded).Write(owned);
        fixed (byte* p = encoded)
        {
            int status = Native.fm_chunk_index_prepare(p, (nuint)encoded.Length, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();
            Handle = new(h);
        }
    }
    internal void Check() => ObjectDisposedException.ThrowIf(Handle.IsClosed, this);
    /// <summary>Releases this owner once. Independently retained cursors and leases remain valid; repeated disposal does not replay release.</summary>
    public void Dispose() { CheckCallbackReentry(); lock (Gate) Handle.Dispose(); }
}
