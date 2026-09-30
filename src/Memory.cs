using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

/// <summary>Limits wrapper-owned capacities, not upstream parser/compressor allocations or process memory.</summary>
public sealed record McapMemoryOptions
{
    public ulong? MaxOwnedInputBytes { get; init; }
    public ulong? MaxPendingBufferBytes { get; init; }
    public ulong MaxRandomAccessCacheBytes { get; init; }
    public ulong? MaxScratchBufferBytes { get; init; }
    public ulong? MaxBufferedSortBytes { get; init; }
    public ulong MaxRetainedBufferBytes { get; init; } = 8 * 1024 * 1024;
}

/// <summary>Per-handle controlled capacities and cumulative wrapper copy/expansion counts. Shared input is included once per view; do not sum related views.</summary>
[StructLayout(LayoutKind.Sequential)]
public readonly record struct McapMemoryStatistics(ulong CurrentControlledBytes, ulong PeakControlledBytes,
    ulong AllocationCount, ulong CopiedBytes, ulong MappedBytes);

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    static extern int fm_memory_statistics(uint kind, SafeHandle handle, out McapMemoryStatistics statistics, out Result r);
    internal static McapMemoryStatistics MemoryStatistics(uint kind, SafeHandle handle)
    {
        int status = fm_memory_statistics(kind, handle, out var statistics, out var r);
        if (status < 0) throw ConsumeError(r);
        return statistics;
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_open_options(uint mode, [MarshalAs(UnmanagedType.I1)] bool ignoreEnd,
        byte* p, nuint n, byte[] config, nuint configLength, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_bytes_options(byte* p, nuint n, byte[] config, nuint configLength, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_open_options(ReaderHandle reader, byte[] config, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_mapped(byte[] config, nuint n, out IntPtr h, out Result r);
}
