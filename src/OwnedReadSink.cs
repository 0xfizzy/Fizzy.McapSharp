using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

// Only internal code sees the event span. It is consumed before the native callback returns.
internal sealed unsafe class OwnedReadSink : IDisposable
{
    internal enum Kind { Record, Message, MessageBody, Schema, ChannelId, Metadata, Attachment }
    readonly Kind kind;
    GCHandle root;
    internal object? Value;
    internal McapMessageHeader Header;
    ExceptionDispatchInfo? error;
    static readonly Native.AcceptOwned Callback = Accept;
    internal OwnedReadSink(Kind kind) { this.kind = kind; }
    // Root only while native code can call back; abandoned enumerators cannot leak a GCHandle.
    internal Lease Acquire() { root = GCHandle.Alloc(this); return new(this); }
    internal readonly struct Lease(OwnedReadSink owner) : IDisposable
    {
        public void Dispose() { if (owner.root.IsAllocated) owner.root.Free(); }
    }
    internal Native.OwnedSink Sink => new() { Context = GCHandle.ToIntPtr(root), Accept = Marshal.GetFunctionPointerForDelegate(Callback) };
    internal static McapChannel CopyChannel(McapChannel channel) => channel with
    {
        Schema = channel.Schema is { } schema ? schema with { Data = schema.Data.AsSpan().ToArray() } : null,
        Metadata = new Dictionary<string, string>(channel.Metadata)
    };
    internal void Reset() { Value = null; error = null; Header = default; }
    internal void ThrowIfError() { var e = error; error = null; e?.Throw(); }
    static int Accept(IntPtr context, byte opcode, Native.NativeHeader* header, byte* data, nuint length, nuint* copied)
    {
        var owner = (OwnedReadSink)GCHandle.FromIntPtr(context).Target!;
        try
        {
            *copied = 0;
            var body = new ReadOnlySpan<byte>(data, checked((int)length));
            owner.Header = new(header->ChannelId, header->Sequence, header->LogTime, header->PublishTime);
            switch (owner.kind)
            {
                case Kind.Record: owner.Value = new McapRecord(opcode, body.ToArray()); *copied = length; break;
                case Kind.MessageBody:
                    if (opcode != 5) return 0;
                    body = body[22..];
                    goto case Kind.Message;
                case Kind.Message: owner.Value = body.ToArray(); *copied = (nuint)body.Length; break;
                case Kind.Schema:
                    var schema = RecordDecoder.Schema(body); owner.Value = schema; *copied = (nuint)schema.Data.Length; break;
                case Kind.ChannelId: owner.Value = RecordDecoder.ChannelId(body); break;
                case Kind.Metadata: owner.Value = RecordDecoder.Metadata(body); break;
                case Kind.Attachment:
                    var attachment = RecordDecoder.Attachment(body); owner.Value = attachment; *copied = (nuint)attachment.Data.Length; break;
            }
            return 0;
        }
        catch (Exception e) { owner.error = ExceptionDispatchInfo.Capture(e); return -1; }
    }
    public void Dispose() { Value = null; if (root.IsAllocated) root.Free(); }
}

internal static partial class Native
{
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal unsafe delegate int AcceptOwned(IntPtr context, byte opcode, NativeHeader* header, byte* data, nuint length, nuint* copied);
    [StructLayout(LayoutKind.Sequential)]
    internal struct OwnedSink { internal IntPtr Context, Accept; }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_owned(ReaderHandle h, byte wanted, OwnedSink sink, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_owned(BufferReaderHandle h, [MarshalAs(UnmanagedType.I1)] bool message, OwnedSink sink, out Result r);
}
