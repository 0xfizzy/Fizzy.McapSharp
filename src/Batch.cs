using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

/// <summary>Consumes a borrowed payload synchronously. Return false to stop after this message.
/// The span expires on return. Do not call back into the originating reader.</summary>
public delegate bool McapMessageVisitor(in McapMessageHeader header, ReadOnlySpan<byte> payload);

[StructLayout(LayoutKind.Sequential)]
public readonly record struct McapPayloadRange(uint Offset, uint Length);
public enum McapBatchStopReason { Capacity, EndOfStream, BufferTooSmall, VisitorStopped }
public readonly record struct McapBatchReadResult(int Count, int BytesWritten, McapBatchStopReason StopReason, ulong RequiredLength);
public readonly record struct McapVisitResult(int Count, McapBatchStopReason StopReason);

/// <summary>A failed batch may have written a prefix and part of its failing record.</summary>
public sealed class McapBatchWriteException : IOException
{
    public int CompletedCount { get; }
    public bool CanContinueWriting { get; }
    internal McapBatchWriteException(int count, Exception error, bool canContinue) : base(error.Message, error)
        => (CompletedCount, CanContinueWriting) = (count, canContinue);
}

internal sealed unsafe class BorrowedReadSink
{
    static readonly Native.AcceptOwned Callback = Accept;
    McapMessageVisitor? visitor;
    bool ignoreStop;
    ExceptionDispatchInfo? error;
    GCHandle root;
    internal bool Active { get; private set; }
    internal void CheckReentry()
    {
        if (Active) throw new InvalidOperationException("Cannot reenter a reader from its message callback.");
    }
    internal Native.OwnedSink Acquire(McapMessageVisitor accept, bool ignoreStop = false)
    {
        CheckReentry();
        root = GCHandle.Alloc(this);
        visitor = accept; this.ignoreStop = ignoreStop; error = null; Active = true;
        return new() { Context = GCHandle.ToIntPtr(root), Accept = Marshal.GetFunctionPointerForDelegate(Callback) };
    }
    internal void Release() { Active = false; visitor = null; if (root.IsAllocated) root.Free(); }
    internal void ThrowIfError() { var e = error; error = null; e?.Throw(); }
    static int Accept(IntPtr context, byte opcode, Native.NativeHeader* header, byte* data, nuint length, nuint* copied)
    {
        var self = (BorrowedReadSink)GCHandle.FromIntPtr(context).Target!;
        try
        {
            *copied = 0;
            var h = new McapMessageHeader(header->ChannelId, header->Sequence, header->LogTime, header->PublishTime);
            return self.visitor!(in h, new ReadOnlySpan<byte>(data, checked((int)length))) || self.ignoreStop ? 0 : 1;
        }
        catch (Exception e) { self.error = ExceptionDispatchInfo.Capture(e); return -1; }
    }
}

public sealed partial class McapWriter
{
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

public sealed partial class McapReadSession
{
    readonly BorrowedReadSink borrowed = new();
    public McapReadStatus ReadNext(McapMessageVisitor visitor)
        => VisitMessages(visitor, 1).Count == 0 ? McapReadStatus.EndOfStream : McapReadStatus.Message;
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

public sealed partial class McapBufferReader
{
    readonly BorrowedReadSink borrowed = new();
    void Check() { borrowed.CheckReentry(); ObjectDisposedException.ThrowIf(handle.IsClosed, this); }
    public McapReadStatus ReadNext(McapMessageVisitor visitor)
        => VisitMessages(visitor, 1).Count == 0 ? McapReadStatus.EndOfStream : McapReadStatus.Message;
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

internal static partial class Native
{
    [StructLayout(LayoutKind.Sequential)]
    internal struct BatchProgress
    {
        internal ulong Count, Bytes, Required, Scanned;
        internal uint PartialValidation, Reserved;
        internal readonly McapBatchReadResult Result(int status) => new(checked((int)Count), checked((int)Bytes), BatchReason(status), Required);
    }
    internal static McapBatchStopReason BatchReason(int status) => status switch {
        1 => McapBatchStopReason.EndOfStream, 2 => McapBatchStopReason.BufferTooSmall,
        3 => McapBatchStopReason.VisitorStopped, _ => McapBatchStopReason.Capacity };
    internal static void CheckBatch(int headers, int ranges)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(headers);
        if (headers != ranges) throw new ArgumentException("Headers and ranges must have the same length.");
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_batch(WriterHandle writer, McapMessageHeader* headers, McapPayloadRange* ranges,
        nuint count, byte* data, nuint length, out nuint completed, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_read_batch(uint kind, SafeHandle reader, McapMessageHeader* headers, McapPayloadRange* ranges,
        nuint count, byte* data, nuint length, out BatchProgress progress, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_visit_messages(uint kind, SafeHandle reader, OwnedSink sink, nuint count, out BatchProgress progress, out Result result);
}
