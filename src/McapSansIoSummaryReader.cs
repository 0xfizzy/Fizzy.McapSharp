namespace Fizzy.McapSharp;

/// <summary>Events produced by the summary protocol.</summary>
public enum McapSummaryEventKind
{
    /// <summary>The selected protocol completed; this is not proof of full-file validation.</summary>
    End,
    /// <summary>Supply up to Length bytes; empty input signals EOF.</summary>
    Read,
    /// <summary>Seek using Origin and Offset, then report the resulting position.</summary>
    Seek,
}
/// <summary>A summary protocol event. Length is in bytes. Positions are relative to the MCAP origin; Seek offsets for Current and End encode signed displacements as unchecked ulong values. Only fields applicable to Kind are meaningful.</summary>
public readonly record struct McapSummaryReadEvent(McapSummaryEventKind Kind, ulong Length, ulong Offset, SeekOrigin Origin);

/// <summary>Caller-driven summary parser. Performs no I/O. Supply requested input before advancing again; buffer-size negotiation preserves pending delivery and other protocol failures are terminal.</summary>
public sealed class McapSansIoSummaryReader : IDisposable
{
    readonly SansIoReaderCore core;
    /// <summary>Creates an independent summary parser. Input requests are serviced by the caller.</summary>
    public McapSansIoSummaryReader(McapSummaryReaderOptions? options = null) => core = SansIoReaderCore.CreateSummary(options);
    /// <summary>Polls the next request or copies delivered bytes. BufferTooSmall reports required Length and retains the same delivery.</summary>
    public McapReadStatus NextEvent(Span<byte> destination, out McapSummaryReadEvent readEvent)
    {
        var status = core.NextEvent(destination, out var e);
        var kind = e.Kind switch
        {
            McapReadEventKind.End => McapSummaryEventKind.End,
            McapReadEventKind.Read => McapSummaryEventKind.Read,
            McapReadEventKind.Seek => McapSummaryEventKind.Seek,
            _ => throw new InvalidOperationException("Unexpected event for this protocol.")
        };
        readEvent = new(kind, e.Length, e.Offset, e.Origin);
        return status;
    }
    /// <summary>Copies input synchronously for the pending request. Indexed chunk input includes its requested absolute position; empty input to a Read request signals EOF.</summary>
    public void SupplyInput(ReadOnlySpan<byte> data, ulong position = 0) => core.SupplyInput(data, position);
    /// <summary>Completes the pending seek with its actual resulting byte position relative to the MCAP origin.</summary>
    public void NotifySeeked(ulong position) => core.NotifySeeked(position);
    /// <summary>Returns an independent complete summary, or null if absent, after the summary protocol completes.</summary>
    public McapSummary? GetSummary() => core.GetSummary();
    /// <summary>Opens a record reader retaining the completed summary independently of this parser.</summary>
    public McapSummaryRecordReader OpenSummaryRecords() => core.OpenSummaryRecords();
    /// <summary>Creates an independent indexed parser from the completed summary; omitted and empty queries use ascending log time.</summary>
    public McapSansIoIndexedReader CreateIndexed(McapIndexedQuery? query = null)
        => new(core.CreateIndexed((query ?? new()).ToQuery(), query?.RecordLengthLimit));
    /// <summary>Releases parser state and reports cleanup failures once; independent child readers remain valid.</summary>
    public void Dispose() => core.Dispose();
}
