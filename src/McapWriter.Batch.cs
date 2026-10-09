using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapWriter
{
    /// <summary>Synchronously writes retained payloads with their original headers, without repacking.
    /// Register the destination channels first. This does not take ownership of the lease.</summary>
    public int WriteBatch(McapMessageBatchLease batch) => WriteLeaseBatch(batch, default, false);

    /// <summary>Writes retained payloads with one replacement header per message. Payload storage
    /// remains owned by the lease; do not dispose it concurrently with this call.</summary>
    public int WriteBatch(McapMessageBatchLease batch, ReadOnlySpan<McapMessageHeader> headers)
        => WriteLeaseBatch(batch, headers, true);

    unsafe int WriteLeaseBatch(McapMessageBatchLease batch, ReadOnlySpan<McapMessageHeader> headers, bool replaceHeaders)
    {
        ArgumentNullException.ThrowIfNull(batch);
        if (replaceHeaders && headers.Length != batch.Count)
            throw new ArgumentException("One replacement header is required per leased message.", nameof(headers));
        var lease = batch.Handle;
        lock (gate)
        {
            Check();
            ObjectDisposedException.ThrowIf(lease.IsClosed, batch);
            bool added = false;
            try
            {
                // Lifetime/argument failures precede the writer failure boundary.
                lease.DangerousAddRef(ref added);
                nuint completed = 0;
                bool safeRejection = false;
                try
                {
                    fixed (McapMessageHeader* h = headers)
                    {
                        int status = Native.fm_writer_lease_batch(handle, lease.DangerousGetHandle(), h,
                            (nuint)headers.Length, out completed, out var result);
                        CheckResult(status, result, ref safeRejection);
                    }
                    return checked((int)completed);
                }
                catch (Exception error)
                {
                    if (!safeRejection) failed = true;
                    throw new McapBatchWriteException(checked((int)completed), error, safeRejection);
                }
            }
            finally { if (added) lease.DangerousRelease(); }
        }
    }

    public unsafe int WriteBatch(ReadOnlySpan<McapMessageHeader> headers, ReadOnlySpan<byte> payloadStorage, ReadOnlySpan<McapPayloadRange> ranges)
    {
        if (headers.Length != ranges.Length) throw new ArgumentException("Headers and ranges must have the same length.");
        foreach (var range in ranges)
            if ((ulong)range.Offset + range.Length > (ulong)payloadStorage.Length) throw new ArgumentOutOfRangeException(nameof(ranges));
        lock (gate)
        {
            Check();
            nuint completed = 0;
            bool safeRejection = false;
            try
            {
                fixed (McapMessageHeader* h = headers)
                fixed (McapPayloadRange* r = ranges)
                fixed (byte* p = payloadStorage)
                {
                    int status = Native.fm_writer_batch(handle, h, r, (nuint)headers.Length, p, (nuint)payloadStorage.Length, out completed, out var result);
                    CheckResult(status, result, ref safeRejection);
                }
                return checked((int)completed);
            }
            catch (Exception e)
            {
                if (!safeRejection) failed = true;
                throw new McapBatchWriteException(checked((int)completed), e, safeRejection);
            }
        }
    }
}
