using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

/// <summary>Consumes a borrowed payload synchronously. Return false to stop after this message.
/// The span expires on return. Do not call back into the originating reader.</summary>
public delegate bool McapMessageVisitor(in McapMessageHeader header, ReadOnlySpan<byte> payload);

/// <summary>Byte offset and length within the payload storage passed to the same batch operation; not a file offset.</summary>
[StructLayout(LayoutKind.Sequential)]
public readonly record struct McapPayloadRange(uint Offset, uint Length);
/// <summary>Why a successful batch read or visit stopped; errors throw instead of returning this status.</summary>
public enum McapBatchStopReason
{
    /// <summary>The message count, header capacity or batch target was reached; further data may remain.</summary>
    Capacity,
    /// <summary>No more messages remain. This alone does not establish full-file validation.</summary>
    EndOfStream,
    /// <summary>The pending message does not fit the caller payload buffer; resize and retry without losing it.</summary>
    BufferTooSmall,
    /// <summary>The visitor returned false after consuming the counted message.</summary>
    VisitorStopped
}
/// <summary>A successful batch result. Count identifies valid header/range entries and BytesWritten the valid payload prefix. On BufferTooSmall, RequiredLength is the entire pending message payload size; retry preserves that message. No result is published on exception.</summary>
public readonly record struct McapBatchReadResult(int Count, int BytesWritten, McapBatchStopReason StopReason, ulong RequiredLength);
/// <summary>Number of completed visitor calls and why traversal stopped. VisitorStopped includes the message whose callback returned false; callback exceptions publish no result.</summary>
public readonly record struct McapVisitResult(int Count, McapBatchStopReason StopReason);

/// <summary>A failed batch may have written a prefix and part of its failing record.</summary>
public sealed class McapBatchWriteException : IOException
{
    /// <summary>Number of complete messages written before the failure. The failing message may be partially written; the batch is not atomic.</summary>
    public int CompletedCount { get; }
    /// <summary>Whether the immediate failure was an audited pre-mutation rejection. Not a live writer state query; I/O failures remain terminal.</summary>
    public bool CanContinueWriting { get; }
    internal McapBatchWriteException(int count, Exception error, bool canContinue) : base(error.Message, error)
        => (CompletedCount, CanContinueWriting) = (count, canContinue);
}
