using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Discriminates parser input requests, delivered records and completion.</summary>
public enum McapReadEventKind
{
    /// <summary>The parser reached the end of its selected scan; this alone does not establish full-file integrity.</summary>
    End,
    /// <summary>Supply up to Length input bytes; empty input signals EOF.</summary>
    Read,
    /// <summary>Seek according to Origin and Offset, then report the resulting byte position from the MCAP origin.</summary>
    Seek,
    /// <summary>A record body is available, identified by Opcode.</summary>
    Record,
    /// <summary>A message payload is available, identified by Header.</summary>
    Message,
    /// <summary>Supply Length compressed Chunk bytes read at Offset bytes from the MCAP origin.</summary>
    ReadChunk
}
/// <summary>A parser request or delivered item; interpret fields according to Kind, not merely a successful read status.</summary>
/// <param name="Kind">Read requests input, Seek requests repositioning, ReadChunk requests indexed compressed chunk data; Record and Message deliver bytes.</param>
/// <param name="Opcode">Record opcode for Record events; not a general event discriminator.</param>
/// <param name="Length">Requested byte count for Read/ReadChunk, delivered body or payload bytes for Record/Message. BufferTooSmall reports required destination capacity.</param>
/// <param name="Offset">Compressed-data byte offset from the MCAP origin for ReadChunk. For Seek with Begin this is a byte offset from the MCAP origin; Current/End encode a signed displacement, recovered with unchecked((long)Offset).</param>
/// <param name="Origin">Seek origin, meaningful only for Seek events.</param>
/// <param name="Header">Message header, meaningful only for Message events.</param>
public readonly record struct McapReadEvent(McapReadEventKind Kind, byte Opcode, ulong Length, ulong Offset, SeekOrigin Origin, McapMessageHeader Header);

/// <summary>Caller-driven official Rust parser. No I/O or borrowed native memory is exposed. Positions are measured from the MCAP origin; when servicing requests on a Stream containing a prefix, translate between these positions and the Stream position using its initial MCAP position.</summary>
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
    /// <summary>Creates an incremental linear parser. MaxRandomAccessCacheBytes is a session-snapshot setting and has no effect here.</summary>
    public static McapSansIoReader CreateLinear(McapReaderOptions? options = null) => new(Protocol.EngineKind.Linear, options ?? new());
    /// <summary>Creates a summary parser whose Seek and Read requests the caller services against a seekable input.</summary>
    public static McapSansIoReader CreateSummary(McapSummaryReaderOptions? options = null) => new(Protocol.EngineKind.Summary, options ?? new());
    /// <summary>Creates an independent indexed parser from this completed summary. Uses topic/time/order filters;
    /// AllowBufferedSort and MaxBufferedSortBytes do not apply because this path never performs buffered fallback.
    /// recordLengthLimit is an optional maximum encoded record-body length in bytes.</summary>
    public McapSansIoReader CreateIndexed(McapQuery? query = null, ulong? recordLengthLimit = null)
    {
        query ??= new();
        if (!Enum.IsDefined(query.Order) || query.StartTime > query.EndTime || (query.Topic is not null && query.Topics is not null)) throw new ArgumentException("Invalid query.", nameof(query));
        lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); return new(Protocol.EngineKind.Indexed, new { query.Topic, query.Topics, query.StartTime, query.EndTime, query.Order, RecordLengthLimit = recordLengthLimit }, this); }
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
                if (status < Protocol.Status.Success) throw Native.ConsumeError(r);
                readEvent = new((McapReadEventKind)e.Kind, (byte)e.Opcode, e.Length, e.Offset, (SeekOrigin)e.Origin, new(e.Header.ChannelId, e.Header.Sequence, e.Header.LogTime, e.Header.PublishTime));
                return status == Protocol.Status.End ? McapReadStatus.EndOfStream : status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    /// <summary>Copies supplied bytes synchronously for the pending request. For Read, supply at most the requested count and an empty span signals EOF. For Seek, pass empty data and the actual new byte position from the MCAP origin. For ReadChunk, pass compressed data and its requested byte offset from the same origin.</summary>
    public unsafe void SupplyInput(ReadOnlySpan<byte> data, ulong position = 0)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* p = data)
            {
                int status = Native.fm_engine_feed(handle, p, (nuint)data.Length, position, out var r);
                if (status < Protocol.Status.Success) throw Native.ConsumeError(r);
            }
        }
    }
    /// <summary>Completes a pending Seek request with the actual byte position from the MCAP origin. Subtract any source prefix from the underlying Stream position.</summary>
    public void NotifySeeked(ulong position) => SupplyInput([], position);
    /// <summary>Supplies indexed compressed Chunk bytes at their byte offset from the MCAP origin. Input is consumed synchronously; protocol errors terminate the parser.</summary>
    public unsafe void InsertChunkData(ulong offset, ReadOnlySpan<byte> compressedData)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            fixed (byte* p = compressedData) { int status = Native.fm_engine_index_control(handle, Protocol.IndexedControl.InsertChunk, offset, p, (nuint)compressedData.Length, out var r); if (status < Protocol.Status.Success) throw Native.ConsumeError(r); }
        }
    }
    /// <summary>Sets the indexed parser's maximum encoded record-body length in bytes; null removes the limit. Requires an indexed parser.</summary>
    public unsafe void SetRecordLengthLimit(ulong? limit)
    {
        lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); int status = Native.fm_engine_index_control(handle, limit.HasValue ? Protocol.IndexedControl.SetRecordLengthLimit : Protocol.IndexedControl.ClearRecordLengthLimit, limit ?? 0, null, 0, out var r); if (status < Protocol.Status.Success) throw Native.ConsumeError(r); }
    }
    /// <summary>Opens an independent record-only cursor over a completed summary, retaining it after parser disposal.</summary>
    public McapBufferReader OpenSummaryRecords() { lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); return Native.SummaryRecords(Protocol.SummarySource.Engine, handle); } }
    /// <summary>Copies the completed summary into independent managed objects, or returns null when the input has no summary.</summary>
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
    /// <summary>Releases parser state and reports native cleanup failures. Repeated disposal is a no-op.</summary>
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
