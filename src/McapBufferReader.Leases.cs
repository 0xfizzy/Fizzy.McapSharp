using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public sealed partial class McapBufferReader
{
    /// <summary>Returns a stable shared batch, or null at EOF. Failure publishes no partial lease and terminates the read. Retain the lease throughout access, including after reader disposal; do not dispose concurrently.</summary>
    public McapMessageBatchLease? ReadBatchLease(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024)
    {
        Native.CheckLeaseRequest(maxMessages, targetPayloadBytes);
        lock (gate)
        {
            Check();
            int status = Native.fm_read_lease(1, handle, (nuint)maxMessages, (nuint)targetPayloadBytes, out var p, out var progress, out var result);
            if (status < 0) throw Native.ConsumeError(result);
            return p == IntPtr.Zero ? null : new(p, checked((int)progress.Count));
        }
    }
}
