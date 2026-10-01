using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Owns stable native message storage. Keep this lease alive until all payload access
/// has finished. Access and disposal must not overlap. Payload spans expire on disposal.</summary>
public sealed class McapMessageBatchLease : IDisposable
{
    readonly MessageLeaseHandle handle;
    internal MessageLeaseHandle Handle => handle;
    public int Count { get; }
    internal McapMessageBatchLease(IntPtr p, int count) { handle = new(p); Count = count; }
    unsafe ReadOnlySpan<byte> Get(int index, out McapMessageHeader header)
    {
        ObjectDisposedException.ThrowIf(handle.IsClosed, this);
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        if (index >= Count) throw new ArgumentOutOfRangeException(nameof(index));
        int status = Native.fm_lease_get(handle, (nuint)index, out var h, out var data, out var length, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime);
        return new(data, checked((int)length));
    }
    public McapMessageHeader GetHeader(int index) { Get(index, out var header); return header; }
    public ReadOnlySpan<byte> GetPayload(int index) => Get(index, out _);
    public void CopyTo(int index, Span<byte> destination) { Get(index, out _).CopyTo(destination); GC.KeepAlive(this); }
    public McapMessageLease RetainMessage(int index)
    {
        ObjectDisposedException.ThrowIf(handle.IsClosed, this);
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        if (index >= Count) throw new ArgumentOutOfRangeException(nameof(index));
        int status = Native.fm_lease_retain(handle, (nuint)index, out var p, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        return new(new(p, 1));
    }
    public void Dispose() => handle.Dispose();
}

/// <summary>An independently retained message. Do not dispose concurrently with payload access.</summary>
public sealed class McapMessageLease : IDisposable
{
    readonly McapMessageBatchLease owner;
    internal McapMessageLease(McapMessageBatchLease owner) => this.owner = owner;
    public McapMessageHeader Header => owner.GetHeader(0);
    public ReadOnlySpan<byte> Payload => owner.GetPayload(0);
    public void CopyTo(Span<byte> destination) => owner.CopyTo(0, destination);
    public void Dispose() => owner.Dispose();
}

public sealed partial class McapReadSession
{
    /// <summary>Returns null at EOF. Target bytes are a soft batch boundary; messages are never split.</summary>
    public McapMessageBatchLease? ReadBatchLease(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024)
    {
        Native.CheckLeaseRequest(maxMessages, targetPayloadBytes);
        if (!messages) throw new InvalidOperationException("This is a record session.");
        lock (gate)
        {
            Check();
            try
            {
                int status = Native.fm_read_lease(0, handle, (nuint)maxMessages, (nuint)targetPayloadBytes, out var p, out var progress, out var result);
                if (status < 0) { var error = Native.ConsumeError(result); handle.Bridge?.ThrowIfError(); throw error; }
                CompleteBatch(status, progress);
                return p == IntPtr.Zero ? null : new(p, checked((int)progress.Count));
            }
            catch { failed = true; throw; }
        }
    }
}
public sealed partial class McapBufferReader
{
    public McapMessageBatchLease? ReadBatchLease(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024)
    {
        Native.CheckLeaseRequest(maxMessages, targetPayloadBytes);
        lock (gate)
        {
            Check();
            int status = Native.fm_read_lease(1, handle, (nuint)maxMessages, (nuint)targetPayloadBytes, out var p, out var progress, out var result);
            if (status < 0) throw Native.ConsumeError(result);
            return p == IntPtr.Zero ? null : new(p, checked((int)progress.Count));
        }
    }
}
internal sealed class MessageLeaseHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal MessageLeaseHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_lease_free(handle);  return true; }
}
internal static partial class Native
{
    internal static void CheckLeaseRequest(int count, int bytes)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(count);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(count, 65536);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(bytes);
    }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_read_lease(uint kind, SafeHandle reader, nuint count, nuint target, out IntPtr lease, out BatchProgress progress, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_lease_get(MessageLeaseHandle lease, nuint index, out NativeHeader header, out byte* data, out nuint length, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_lease_retain(MessageLeaseHandle lease, nuint index, out IntPtr retained, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_lease_free(IntPtr lease);
}
