using System.Runtime.InteropServices;
namespace Fizzy.McapSharp;

/// <summary>Limits only the snapshot cache, not upstream allocations or live leases.</summary>
public sealed record McapIndexSnapshotOptions
{
    /// <summary>Local LRU allowance for retained chunk storage, descriptors, keys and index bytes.
    /// Zero disables retention; oversized entries load without retention. Excludes input storage,
    /// parser temporaries and external leases. Eviction does not invalidate leases.</summary>
    public ulong MaxRandomAccessCacheBytes { get; init; }
}

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_bytes_options(byte* p, nuint n, byte[] config, nuint configLength, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_open_options(ReaderHandle reader, byte[] config, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_mapped(byte[] config, nuint n, out IntPtr h, out Result r);
}
