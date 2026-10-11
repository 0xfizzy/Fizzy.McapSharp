namespace Fizzy.McapSharp;

/// <summary>Indexed message selection. Omitted and empty queries use ascending log time. Requires complete indexes and never falls back to scanning or buffered sorting.</summary>
public sealed record McapIndexedQuery
{
    /// <summary>Optional exact topic set; mutually exclusive with Topic. Null selects all topics; an empty collection selects none.</summary>
    public IReadOnlyCollection<string>? Topics { get; init; }
    /// <summary>Optional exact topic filter.</summary>
    public string? Topic { get; init; }
    /// <summary>Inclusive log-time lower bound in caller-defined nanoseconds.</summary>
    public ulong? StartTime { get; init; }
    /// <summary>Exclusive log-time upper bound in caller-defined nanoseconds.</summary>
    public ulong? EndTime { get; init; }
    /// <summary>Indexed traversal order. Defaults to ascending log time; equal-time order is unspecified.</summary>
    public McapReadOrder Order { get; init; } = McapReadOrder.LogTime;
    /// <summary>Maximum encoded record-body length in bytes. Null leaves the upstream limit unset; this is not a total-memory bound.</summary>
    public ulong? RecordLengthLimit { get; init; }
    internal McapQuery ToQuery() => new() { Topic = Topic, Topics = Topics, StartTime = StartTime, EndTime = EndTime, Order = Order };
}
