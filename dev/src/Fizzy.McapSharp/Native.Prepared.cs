using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

internal sealed class PreparedChannelHandle : OwnedNativeHandle
{
    internal PreparedChannelHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_channel_release(value, out result);
}

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_channel_prepare(byte[] req, nuint n, byte* data, nuint len, out IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_full_message(WriterHandle w, PreparedChannelHandle c, NativeHeader* h, byte* data, nuint len, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_private(WriterHandle w, byte opcode, [MarshalAs(UnmanagedType.I1)] bool chunks, byte* data, nuint len, out Result result);
}


internal sealed class OperationHandle : OwnedNativeHandle
{
    internal OperationHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_operation_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_operation_prepare(uint op, byte[] req, nuint n, byte* data, nuint len, out IntPtr p, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_prepared(WriterHandle h, OperationHandle op, byte* data, nuint len, out Result r);
}
