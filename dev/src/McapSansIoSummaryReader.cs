namespace Fizzy.McapSharp;

/// <summary>Events produced by the summary protocol.</summary>
public enum McapSummaryEventKind
{
    /// <summary>The selected protocol completed; this is not proof of full-file validation.</summary>
    End,
    /// <summary>Supply up to Length bytes; empty input signals EOF.</summary>
    Read,
    /// <summary>Seek using the Seek request, then report the resulting position.</summary>
    Seek,
}
/// <summary>A summary protocol event. Length applies to Read; Seek applies to Seek. Positions are relative to the MCAP origin. Other fields are not meaningful.</summary>
public readonly record struct McapSummaryReadEvent(McapSummaryEventKind Kind, ulong Length, McapSeekRequest Seek);

/// <summary>Caller-driven summary parser. Performs no I/O. Supply requested input or acknowledge the requested seek before advancing again; protocol failures are terminal.</summary>
public sealed class McapSansIoSummaryReader : IDisposable
{
    readonly SansIoReaderCore core;
    /// <summary>Creates an independent summary parser. Input requests are serviced by the caller.</summary>
    public McapSansIoSummaryReader(McapSummaryReaderOptions? options = null) => core = SansIoReaderCore.CreateSummary(options);
    /// <summary>Polls the next Read or Seek request, or returns EndOfStream. This protocol delivers no record bodies and never returns BufferTooSmall.</summary>
    public McapReadStatus NextEvent(out McapSummaryReadEvent readEvent)
    {
        var status = core.NextEvent([], out var e);
        var kind = e.Kind switch
        {
            McapReadEventKind.End => McapSummaryEventKind.End,
            McapReadEventKind.Read => McapSummaryEventKind.Read,
            McapReadEventKind.Seek => McapSummaryEventKind.Seek,
            _ => throw new InvalidOperationException("Unexpected event for this protocol.")
        };
        readEvent = new(kind, e.Length, new McapSeekRequest(e.Origin, e.Offset));
        return status;
    }
    /// <summary>Copies up to the requested bytes for a pending Read. Empty input signals EOF. Other pending event kinds are terminal protocol errors.</summary>
    public void SupplyInput(ReadOnlySpan<byte> data) => core.SupplyInput(data);
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
