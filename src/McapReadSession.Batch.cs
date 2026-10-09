using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapReadSession
{
    public McapReadStatus ReadNext(McapMessageVisitor visitor)
        => VisitMessages(visitor, 1).Count == 0 ? McapReadStatus.EndOfStream : McapReadStatus.Success;
    /// <summary>Invokes the visitor synchronously without reader reentry. On failure previously executed callback effects are not rolled back; no successful prefix count is returned and the failed read cannot continue.</summary>
    public McapVisitResult VisitMessages(McapMessageVisitor visitor, int maxMessages = 256)
    {
        ArgumentNullException.ThrowIfNull(visitor);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(maxMessages);
        if (!messages) throw new InvalidOperationException("This is a record session.");
        lock (gate)
        {
            Check();
            try
            {
                var sink = borrowed.Acquire(visitor);
                int status = Native.fm_visit_messages(0, handle, sink, (nuint)maxMessages, out var progress, out var result);
                if (status < 0) { var error = Native.ConsumeError(result); borrowed.ThrowIfError(); handle.Bridge?.ThrowIfError(); throw error; }
                CompleteBatch(status, progress);
                return new(checked((int)progress.Count), Native.BatchReason(status));
            }
            catch { failed = true; throw; }
            finally { borrowed.Release(); }
        }
    }
    /// <summary>Copies messages into caller-owned buffers. On exception discard all output from this call: buffers may contain an unreported prefix and cannot be retried. BufferTooSmall is a successful negotiation result with a valid reported prefix and a retained pending message.</summary>
    public unsafe McapBatchReadResult ReadBatch(Span<McapMessageHeader> headers, Span<McapPayloadRange> ranges, Span<byte> payloadStorage)
    {
        Native.CheckBatch(headers.Length, ranges.Length);
        if (!messages) throw new InvalidOperationException("This is a record session.");
        lock (gate)
        {
            Check();
            try
            {
                fixed (McapMessageHeader* h = headers)
                fixed (McapPayloadRange* r = ranges)
                fixed (byte* p = payloadStorage)
                {
                    int status = Native.fm_read_batch(0, handle, h, r, (nuint)headers.Length, p, (nuint)payloadStorage.Length, out var progress, out var result);
                    if (status < 0) { var error = Native.ConsumeError(result); handle.Bridge?.ThrowIfError(); throw error; }
                    CompleteBatch(status, progress);
                    return progress.Result(status);
                }
            }
            catch { failed = true; throw; }
        }
    }
    void CompleteBatch(int status, Native.BatchProgress progress)
    {
        if (status != 1) return;
        ended = true;
        fullyValidated = strict && progress.PartialValidation == 0 && !topLevel;
        ScannedRecordCount = progress.Scanned;
    }
}
