using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public sealed partial class McapReadSession
{
    /// <summary>Returns null at EOF. Checks the soft payload target after each whole message, so a batch may exceed it. The target does not bound backing capacity or outstanding leases.
    /// Returns a stable shared batch, or null at EOF. Failure publishes no partial lease and terminates the read. Retain the lease throughout access, including after reader disposal; do not dispose concurrently.</summary>
    public McapMessageBatchLease? ReadBatchLease(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024)
    {
        Native.CheckLeaseRequest(maxMessages, targetPayloadBytes);
        if (!messages) throw new InvalidOperationException("This is a record session.");
        lock (gate)
        {
            Check();
            try
            {
                int status = Native.fm_read_lease(Protocol.ReaderKind.Session, handle, (nuint)maxMessages, (nuint)targetPayloadBytes, out var p, out var progress, out var result);
                if (status < Protocol.Status.Success) { var error = Native.ConsumeError(result); handle.Bridge?.ThrowIfError(); throw error; }
                CompleteBatch(status, progress);
                return p == IntPtr.Zero ? null : new(p, checked((int)progress.Count));
            }
            catch { failed = true; throw; }
        }
    }
}
