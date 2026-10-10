namespace Fizzy.McapSharp;

/// <summary>Message traversal ordering; equal log-time ordering is unspecified.</summary>
public enum McapReadOrder
{
    /// <summary>Ascending message log time; may require indexes or buffered sorting.</summary>
    LogTime,
    /// <summary>Descending message log time; may require indexes or buffered sorting.</summary>
    ReverseLogTime,
    /// <summary>Recording order, suitable for incremental scans without global sorting.</summary>
    File
}

/// <summary>Message selection with inclusive StartTime and exclusive EndTime. Topic and Topics are mutually exclusive. A null query uses file order; an explicit query defaults to ascending log time.</summary>
public sealed record McapQuery
{
    /// <summary>Optional fallback collection allowance: selected payload lengths plus descriptor-array capacity in bytes. Null disables the allowance. Shared ranges count separately; retained backing, compaction overlap and parser/codec memory are excluded. Does not apply to indexed reading.</summary>
    public ulong? MaxBufferedSortBytes { get; init; }
    /// <summary>Allows scanning and buffering all selected messages when time ordering cannot use indexes.</summary>
    public bool AllowBufferedSort { get; init; } = true;
    /// <summary>Message ordering; defaults to ascending log time. Time ordering is monotonic; equal-time order is unspecified. File order permits incremental scanning.</summary>
    public McapReadOrder Order { get; init; } = McapReadOrder.LogTime;
    /// <summary>Optional exact topic set; mutually exclusive with Topic. Null selects all topics, while an empty set selects none.</summary>
    public IReadOnlyCollection<string>? Topics { get; init; }
    /// <summary>Optional exact topic filter; null selects all topics.</summary>
    public string? Topic { get; init; }
    /// <summary>Inclusive log-time lower bound in caller-defined nanoseconds; null means unbounded.</summary>
    public ulong? StartTime { get; init; }
    /// <summary>Exclusive log-time upper bound in caller-defined nanoseconds; null means unbounded.</summary>
    public ulong? EndTime { get; init; }
}
