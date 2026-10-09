using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

internal static partial class Native
{
    internal static void CheckLeaseRequest(int count, int bytes)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(count);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(count, 65536);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(bytes);
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_read_lease(uint kind, SafeHandle reader, nuint count, nuint target, out IntPtr lease, out BatchProgress progress, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_lease_get(MessageLeaseHandle lease, nuint index, out NativeHeader header, out byte* data, out nuint length, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_lease_retain(MessageLeaseHandle lease, nuint index, out IntPtr retained, out Result result);
}
