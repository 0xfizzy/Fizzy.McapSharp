using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

internal sealed class MessageLeaseHandle : OwnedNativeHandle
{
    internal MessageLeaseHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_lease_release(value, out result);
}
