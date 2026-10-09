using System.Runtime.InteropServices;
using System.Threading.Tasks.Sources;

namespace Fizzy.McapSharp;

public sealed partial class McapSansIoReader
{
    internal (int Status, McapMessageBatchLease? Batch, ulong Needed) LeaseStep(int count, int target)
    {
        int status = Native.fm_engine_lease_step(handle, (nuint)count, (nuint)target, out var p, out var e, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        return (status, p == IntPtr.Zero ? null : new(p, checked((int)result.Value)), e.Length);
    }
}
