using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

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
        Protocol.Status.End => McapBatchStopReason.EndOfStream, Protocol.Status.BufferTooSmall => McapBatchStopReason.BufferTooSmall,
        Protocol.BatchStatus.VisitorStopped => McapBatchStopReason.VisitorStopped, _ => McapBatchStopReason.Capacity };
    internal static void CheckBatch(int headers, int ranges)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(headers);
        if (headers != ranges) throw new ArgumentException("Headers and ranges must have the same length.");
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_batch(WriterHandle writer, McapMessageHeader* headers, McapPayloadRange* ranges,
        nuint count, byte* data, nuint length, out nuint completed, out Result result);
    // batch is protected by WriteLeaseBatch's explicit SafeHandle reference for the entire call.
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_lease_batch(WriterHandle writer, IntPtr batch, McapMessageHeader* headers,
        nuint headerCount, out nuint completed, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_read_batch(uint kind, SafeHandle reader, McapMessageHeader* headers, McapPayloadRange* ranges,
        nuint count, byte* data, nuint length, out BatchProgress progress, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_visit_messages(uint kind, SafeHandle reader, OwnedSink sink, nuint count, out BatchProgress progress, out Result result);
}
