using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

internal static partial class Native
{
    [StructLayout(LayoutKind.Sequential)]
    internal struct SeekRequest { internal IntPtr Index; internal ulong Time, Offset; }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_seek_batch(SnapshotHandle snapshot, SeekRequest* requests, nuint count, out IntPtr batch, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_cache_statistics(SnapshotHandle snapshot, out ulong hits, out ulong loads, out Result result);
}
