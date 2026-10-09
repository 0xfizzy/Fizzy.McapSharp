using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

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
            return self.visitor!(in h, new ReadOnlySpan<byte>(data, checked((int)length))) || self.ignoreStop ? Protocol.CallbackStatus.Accepted : Protocol.CallbackStatus.Stop;
        }
        catch (Exception e) { self.error = ExceptionDispatchInfo.Capture(e); return Protocol.CallbackStatus.Error; }
    }
}
