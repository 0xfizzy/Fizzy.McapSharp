namespace Fizzy.McapSharp;

/// <summary>Events produced by the indexed protocol.</summary>
public enum McapIndexedEventKind
{
    /// <summary>The selected protocol completed; this is not proof of full-file validation.</summary>
    End,
    /// <summary>The next message payload is available; Header identifies it.</summary>
    Message,
    /// <summary>Supply Length compressed bytes from the absolute chunk Offset.</summary>
    ReadChunk,
}
/// <summary>An indexed protocol event. ReadChunk uses Length and Offset for compressed data excluding the Chunk header, relative to the MCAP origin. Message uses Length and Header.</summary>
public readonly record struct McapIndexedReadEvent(McapIndexedEventKind Kind, ulong Length, ulong Offset, McapMessageHeader Header);

/// <summary>Caller-driven indexed parser. Performs no I/O. Supply requested input before advancing again; buffer-size negotiation preserves pending delivery and other protocol failures are terminal.</summary>
public sealed class McapSansIoIndexedReader : IDisposable
{
    readonly SansIoReaderCore core;
    internal McapSansIoIndexedReader(SansIoReaderCore core) => this.core = core;
    /// <summary>Polls the next request or copies delivered bytes. BufferTooSmall reports required Length and retains the same delivery.</summary>
    public McapReadStatus NextEvent(Span<byte> destination, out McapIndexedReadEvent readEvent)
    {
        var status = core.NextEvent(destination, out var e);
        var kind = e.Kind switch
        {
            McapReadEventKind.End => McapIndexedEventKind.End,
            McapReadEventKind.Message => McapIndexedEventKind.Message,
            McapReadEventKind.ReadChunk => McapIndexedEventKind.ReadChunk,
            _ => throw new InvalidOperationException("Unexpected event for this protocol.")
        };
        readEvent = new(kind, e.Length, e.Offset, e.Header);
        return status;
    }
    /// <summary>Supplies exactly the requested compressed data bytes, excluding the outer Chunk header, at the requested absolute offset. Complete short reads before calling. Input is consumed synchronously; invalid protocol use terminates this parser.</summary>
    public void InsertChunkData(ulong offset, ReadOnlySpan<byte> data) => core.InsertChunkData(offset, data);
    /// <summary>Changes the upstream indexed record-length limit in bytes for subsequent checks. Initial configuration also checks indexed compressed and decompressed chunk sizes; changing this value does not repeat construction-time checks. Null removes the limit.</summary>
    public void SetRecordLengthLimit(ulong? limit) => core.SetRecordLengthLimit(limit);
    /// <summary>Releases parser state and reports cleanup failures once.</summary>
    public void Dispose() => core.Dispose();
}
