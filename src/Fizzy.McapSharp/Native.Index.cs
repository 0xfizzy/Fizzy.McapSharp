using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

internal sealed class SnapshotHandle : OwnedNativeHandle
{
    internal SnapshotHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_snapshot_release(value, out result);
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
    internal static extern unsafe int fm_parse_record(byte op, byte* p, nuint n, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_record_into(ReaderHandle h, ulong offset, byte* p, nuint n, out byte opcode, out Result r);
}

internal sealed class PreparedChunkIndexHandle : OwnedNativeHandle
{
    internal PreparedChunkIndexHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_chunk_index_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_chunk_index_prepare(byte* data, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_prepared_call(SnapshotHandle h, uint op, PreparedChunkIndexHandle index, ulong time, ulong offset, byte* dest, nuint n, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_prepared_chunk_reader(SnapshotHandle h, PreparedChunkIndexHandle index, out IntPtr reader, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_message_owned(SnapshotHandle h, byte* data, nuint n, IntPtr prepared, ulong time, ulong offset, OwnedSink sink, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_snapshot_record_owned(SnapshotHandle h, uint op, byte* data, nuint n, OwnedSink sink, out Result r);
}

