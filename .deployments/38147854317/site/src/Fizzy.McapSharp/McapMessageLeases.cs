using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Owns stable native message storage. Keep this lease alive until all payload access
/// has finished. Access and disposal must not overlap. Payload spans expire on disposal.
/// A payload slice can retain an entire chunk, copied input or mapping; payload length is not retained capacity.</summary>
public sealed class McapMessageBatchLease : IDisposable
{
    readonly MessageLeaseHandle handle;
    internal MessageLeaseHandle Handle => handle;
    /// <summary>Number of retained messages; valid indexes range from zero to Count minus one.</summary>
    public int Count { get; }
    internal McapMessageBatchLease(IntPtr p, int count) { handle = new(p); Count = count; }
    unsafe ReadOnlySpan<byte> Get(int index, out McapMessageHeader header)
    {
        ObjectDisposedException.ThrowIf(handle.IsClosed, this);
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        if (index >= Count) throw new ArgumentOutOfRangeException(nameof(index));
        int status = Native.fm_lease_get(handle, (nuint)index, out var h, out var data, out var length, out var result);
        if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
        header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime);
        return new(data, checked((int)length));
    }
    /// <summary>Returns a value copy of the selected header. Requires a live lease and valid index.</summary>
    public McapMessageHeader GetHeader(int index) { Get(index, out var header); return header; }
    /// <summary>Borrows the selected payload. Keep this lease alive and undisposed until all span access ends.</summary>
    public ReadOnlySpan<byte> GetPayload(int index) => Get(index, out _);
    /// <summary>Copies the selected payload into caller storage; destination must fit the entire payload. The copy survives lease disposal.</summary>
    public void CopyTo(int index, Span<byte> destination) { Get(index, out _).CopyTo(destination); GC.KeepAlive(this); }
    /// <summary>Shares the selected message's backing with an independent lease; does not copy or trim storage.
    /// Dispose the returned lease separately. Other owners may continue retaining the same backing.</summary>
    public McapMessageLease RetainMessage(int index)
    {
        ObjectDisposedException.ThrowIf(handle.IsClosed, this);
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        if (index >= Count) throw new ArgumentOutOfRangeException(nameof(index));
        int status = Native.fm_lease_retain(handle, (nuint)index, out var p, out var result);
        if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
        return new(new(p, 1));
    }
    /// <summary>Releases this lease reference once and invalidates borrowed payload spans. Other independent leases retain their own references; do not overlap disposal with access.</summary>
    public void Dispose() => handle.Dispose();
}

/// <summary>An independently retained message. Do not dispose concurrently with payload access.</summary>
public sealed class McapMessageLease : IDisposable
{
    readonly McapMessageBatchLease owner;
    internal McapMessageLease(McapMessageBatchLease owner) => this.owner = owner;
    /// <summary>Returns a value copy of the retained message header; requires a live lease.</summary>
    public McapMessageHeader Header => owner.GetHeader(0);
    /// <summary>Borrows stable payload bytes until this lease is disposed. Keep the lease alive throughout access.</summary>
    public ReadOnlySpan<byte> Payload => owner.GetPayload(0);
    /// <summary>Copies the entire payload into caller-owned storage. The destination must fit; the copy survives disposal.</summary>
    public void CopyTo(Span<byte> destination) => owner.CopyTo(0, destination);
    /// <summary>Releases this lease reference once and invalidates borrowed payload spans. Other independent leases retain their own references; do not overlap disposal with access.</summary>
    public void Dispose() => owner.Dispose();
}
