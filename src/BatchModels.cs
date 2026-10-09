using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

/// <summary>Consumes a borrowed payload synchronously. Return false to stop after this message.
/// The span expires on return. Do not call back into the originating reader.</summary>
public delegate bool McapMessageVisitor(in McapMessageHeader header, ReadOnlySpan<byte> payload);

/// <summary>Byte offset and length within the payload storage passed to the same batch operation; not a file offset.</summary>
[StructLayout(LayoutKind.Sequential)]
public readonly record struct McapPayloadRange(uint Offset, uint Length);
public enum McapBatchStopReason { Capacity, EndOfStream, BufferTooSmall, VisitorStopped }
/// <summary>A successful batch result. Count identifies valid header/range entries and BytesWritten the valid payload prefix. On BufferTooSmall, RequiredLength is the entire pending message payload size; retry preserves that message. No result is published on exception.</summary>
public readonly record struct McapBatchReadResult(int Count, int BytesWritten, McapBatchStopReason StopReason, ulong RequiredLength);
/// <summary>Number of completed visitor calls and why traversal stopped. VisitorStopped includes the message whose callback returned false; callback exceptions publish no result.</summary>
public readonly record struct McapVisitResult(int Count, McapBatchStopReason StopReason);

/// <summary>A failed batch may have written a prefix and part of its failing record.</summary>
public sealed class McapBatchWriteException : IOException
{
    public int CompletedCount { get; }
    public bool CanContinueWriting { get; }
    internal McapBatchWriteException(int count, Exception error, bool canContinue) : base(error.Message, error)
        => (CompletedCount, CanContinueWriting) = (count, canContinue);
}
