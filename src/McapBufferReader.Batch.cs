using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapBufferReader
{
    public McapReadStatus ReadNext(McapMessageVisitor visitor)
        => VisitMessages(visitor, 1).Count == 0 ? McapReadStatus.EndOfStream : McapReadStatus.Success;
    /// <summary>Invokes the visitor synchronously without reader reentry. On failure previously executed callback effects are not rolled back; no successful prefix count is returned and the failed read cannot continue.</summary>
    public McapVisitResult VisitMessages(McapMessageVisitor visitor, int maxMessages = 256)
    {
        ArgumentNullException.ThrowIfNull(visitor);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(maxMessages);
        lock (gate)
        {
            Check();
            try
            {
                int status = Native.fm_visit_messages(1, handle, borrowed.Acquire(visitor), (nuint)maxMessages, out var progress, out var result);
                if (status < 0) { var error = Native.ConsumeError(result); borrowed.ThrowIfError(); throw error; }
                return new(checked((int)progress.Count), Native.BatchReason(status));
            }
            finally { borrowed.Release(); }
        }
    }
    /// <summary>Copies messages into caller-owned buffers. On exception discard all output from this call: buffers may contain an unreported prefix and cannot be retried. BufferTooSmall is a successful negotiation result with a valid reported prefix and a retained pending message.</summary>
    public unsafe McapBatchReadResult ReadBatch(Span<McapMessageHeader> headers, Span<McapPayloadRange> ranges, Span<byte> payloadStorage)
    {
        Native.CheckBatch(headers.Length, ranges.Length);
        lock (gate)
        {
            Check();
            fixed (McapMessageHeader* h = headers)
            fixed (McapPayloadRange* r = ranges)
            fixed (byte* p = payloadStorage)
            {
                int status = Native.fm_read_batch(1, handle, h, r, (nuint)headers.Length, p, (nuint)payloadStorage.Length, out var progress, out var result);
                if (status < 0) throw Native.ConsumeError(result);
                return progress.Result(status);
            }
        }
    }
}
