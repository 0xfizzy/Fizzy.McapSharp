namespace Fizzy.McapSharp;

/// <summary>Events produced by the indexed protocol.</summary>
public enum McapIndexedEventKind
{
    /// <summary>The selected protocol completed; this is not proof of full-file validation.</summary>
    End,
    /// <summary>Supply up to Length bytes; empty input signals EOF.</summary>
    Read,
    /// <summary>Seek using Origin and Offset, then report the resulting position.</summary>
    Seek,
    /// <summary>The next message payload is available; Header identifies it.</summary>
    Message,
    /// <summary>Supply Length compressed bytes from the absolute chunk Offset.</summary>
    ReadChunk,
}
/// <summary>A indexed protocol event. Length is in bytes. Positions are relative to the MCAP origin; Seek offsets for Current and End encode signed displacements as unchecked ulong values. Only fields applicable to Kind are meaningful.</summary>
public readonly record struct McapIndexedReadEvent(McapIndexedEventKind Kind, ulong Length, ulong Offset, SeekOrigin Origin, McapMessageHeader Header);

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
            McapReadEventKind.Read => McapIndexedEventKind.Read,
            McapReadEventKind.Seek => McapIndexedEventKind.Seek,
            McapReadEventKind.Message => McapIndexedEventKind.Message,
            McapReadEventKind.ReadChunk => McapIndexedEventKind.ReadChunk,
            _ => throw new InvalidOperationException("Unexpected event for this protocol.")
        };
        readEvent = new(kind, e.Length, e.Offset, e.Origin, e.Header);
        return status;
    }
    /// <summary>Copies input synchronously for the pending request. Indexed chunk input includes its requested absolute position; empty input to a Read request signals EOF.</summary>
    public void SupplyInput(ReadOnlySpan<byte> data, ulong position = 0) => core.SupplyInput(data, position);
    /// <summary>Completes the pending seek with its actual resulting byte position relative to the MCAP origin.</summary>
    public void NotifySeeked(ulong position) => core.NotifySeeked(position);
    /// <summary>Supplies compressed chunk bytes at the requested absolute offset. Input is consumed synchronously; invalid protocol use terminates this parser.</summary>
    public void InsertChunkData(ulong offset, ReadOnlySpan<byte> data) => core.InsertChunkData(offset, data);
    /// <summary>Changes the maximum encoded record-body length in bytes. Null removes the limit.</summary>
    public void SetRecordLengthLimit(ulong? limit) => core.SetRecordLengthLimit(limit);
    /// <summary>Releases parser state and reports cleanup failures once; independent child readers remain valid.</summary>
    public void Dispose() => core.Dispose();
}
