using System.Runtime.InteropServices;
namespace Fizzy.McapSharp;

/// <summary>Limits only the snapshot cache, not upstream allocations or live leases.</summary>
public sealed record McapIndexSnapshotOptions
{
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
