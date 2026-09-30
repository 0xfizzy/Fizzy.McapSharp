using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    static extern int fm_summary_records(uint kind, IntPtr source, out IntPtr h, out Result r);
    internal static McapBufferReader SummaryRecords(uint kind, SafeHandle handle)
    {
        bool added = false;
        try
        {
            handle.DangerousAddRef(ref added);
            var status = fm_summary_records(kind, handle.DangerousGetHandle(), out var h, out var r);
            Consume(status, r).Json?.Dispose();
            return new(h);
        }
        finally { if (added) handle.DangerousRelease(); }
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_channel(SnapshotHandle h, ushort id, out Result r);
}
public sealed partial class McapWriter
{
    public McapBufferReader OpenSummaryRecords()
    {
        lock (gate)
        {
            CheckCompleted();
            return Native.SummaryRecords(2, handle);
        }
    }
}
