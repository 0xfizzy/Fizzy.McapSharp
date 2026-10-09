using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public enum McapReadEventKind { End, Read, Seek, Record, Message, ReadChunk }
/// <summary>A parser request or delivered item; interpret fields according to Kind, not merely a successful read status.</summary>
/// <param name="Kind">Read requests input, Seek requests repositioning, ReadChunk requests indexed compressed chunk data; Record and Message deliver bytes.</param>
/// <param name="Opcode">Record opcode for Record events; not a general event discriminator.</param>
/// <param name="Length">Requested byte count for Read/ReadChunk, delivered body or payload bytes for Record/Message. BufferTooSmall reports required destination capacity.</param>
/// <param name="Offset">Absolute compressed-data offset for ReadChunk. For Seek with Begin this is an absolute offset; Current/End encode a signed displacement, recovered with unchecked((long)Offset).</param>
/// <param name="Origin">Seek origin, meaningful only for Seek events.</param>
/// <param name="Header">Message header, meaningful only for Message events.</param>
public readonly record struct McapReadEvent(McapReadEventKind Kind, byte Opcode, ulong Length, ulong Offset, SeekOrigin Origin, McapMessageHeader Header);

/// <summary>Caller-driven official Rust parser. No I/O or borrowed native memory is exposed.</summary>
public sealed partial class McapSansIoReader : IDisposable
{
    readonly EngineHandle handle;
    readonly object gate = new();
    bool disposed;
    unsafe McapSansIoReader(uint kind, object options, McapSansIoReader? summary = null)
    {
        Native.EnsureAvailable();
        var req = Native.Request(options);
        bool added = false;
        try
        {
            summary?.handle.DangerousAddRef(ref added);
            int status = Native.fm_engine_open(kind, req, (nuint)req.Length, summary?.handle.DangerousGetHandle() ?? IntPtr.Zero, out var p, out var r);
            Native.Consume(status, r).Json?.Dispose();

            handle = new(p);
        }
        finally { if (added) summary!.handle.DangerousRelease(); }
    }
    public static McapSansIoReader CreateLinear(McapReaderOptions? options = null) => new(0, options ?? new());
    public static McapSansIoReader CreateSummary(McapSummaryReaderOptions? options = null) => new(1, options ?? new());
    public McapSansIoReader CreateIndexed(McapQuery? query = null, ulong? recordLengthLimit = null)
    {
        query ??= new();
        if (!Enum.IsDefined(query.Order) || query.StartTime > query.EndTime || (query.Topic is not null && query.Topics is not null)) throw new ArgumentException("Invalid query.", nameof(query));
        lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); return new(2, new { query.Topic, query.Topics, query.StartTime, query.EndTime, query.Order, RecordLengthLimit = recordLengthLimit }, this); }
    }
    /// <summary>Advances the protocol and copies delivered record/message bytes into caller storage. Read/Seek/ReadChunk requests require the corresponding input notification before advancing. BufferTooSmall retains the same pending delivery for retry.</summary>
    public unsafe McapReadStatus NextEvent(Span<byte> destination, out McapReadEvent readEvent)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* p = destination)
            {
                int status = Native.fm_engine_next(handle, p, (nuint)destination.Length, out var e, out var r);
                if (status < 0) throw Native.ConsumeError(r);
                readEvent = new((McapReadEventKind)e.Kind, (byte)e.Opcode, e.Length, e.Offset, (SeekOrigin)e.Origin, new(e.Header.ChannelId, e.Header.Sequence, e.Header.LogTime, e.Header.PublishTime));
                return status == 1 ? McapReadStatus.EndOfStream : status == 2 ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    /// <summary>Copies supplied bytes synchronously for the pending request. For Read, supply at most the requested count and an empty span signals EOF. For Seek, pass empty data and the actual new absolute position. For ReadChunk, pass compressed data and its requested absolute offset.</summary>
    public unsafe void SupplyInput(ReadOnlySpan<byte> data, ulong position = 0)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* p = data)
            {
                int status = Native.fm_engine_feed(handle, p, (nuint)data.Length, position, out var r);
                if (status < 0) throw Native.ConsumeError(r);
            }
        }
    }
    /// <summary>Completes a pending Seek request with the actual absolute position returned by the underlying source.</summary>
    public void NotifySeeked(ulong position) => SupplyInput([], position);
    public unsafe void InsertChunkData(ulong offset, ReadOnlySpan<byte> compressedData)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* p = compressedData) { int status = Native.fm_engine_index_control(handle, 0, offset, p, (nuint)compressedData.Length, out var r); if (status < 0) throw Native.ConsumeError(r); }
        }
    }
    public unsafe void SetRecordLengthLimit(ulong? limit)
    {
        lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); int status = Native.fm_engine_index_control(handle, limit.HasValue ? 1u : 2u, limit ?? 0, null, 0, out var r); if (status < 0) throw Native.ConsumeError(r); }
    }
    public McapBufferReader OpenSummaryRecords() { lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); return Native.SummaryRecords(1, handle); } }
    public McapSummary? GetSummary()
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            var status = Native.fm_engine_summary(handle, out var r);
            var result = Native.Consume(status, r);
            using var j = result.Json;
            return j?.RootElement.Deserialize<McapSummary>(JsonSupport.Options);
        }
    }
    internal bool NativeReleased => handle.NativeReleased;
    public void Dispose() { lock (gate) { if (disposed) return; disposed = true; handle.Dispose(); } }
}

internal sealed class EngineHandle : OwnedNativeHandle
{
    internal EngineHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_engine_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_engine_index_control(EngineHandle h, uint op, ulong value, byte* data, nuint n, out Result r);
    [StructLayout(LayoutKind.Sequential)]
    internal struct ReadEvent { public uint Kind, Opcode; public ulong Length, Offset; public uint Origin, Reserved; public NativeHeader Header; }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_open(uint kind, byte[] req, nuint n, IntPtr summary, out IntPtr handle, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_engine_next(EngineHandle h, byte* dest, nuint capacity, out ReadEvent e, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_engine_feed(EngineHandle h, byte* data, nuint length, ulong position, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_summary(EngineHandle h, out Result r);
}
