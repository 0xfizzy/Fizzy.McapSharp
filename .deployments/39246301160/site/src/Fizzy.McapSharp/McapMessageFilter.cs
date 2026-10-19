namespace Fizzy.McapSharp;

/// <summary>Selects messages during a sequential file-order scan. Omitted and empty filters are equivalent; filtering never selects indexes or requests sorting.</summary>
public sealed record McapMessageFilter
{
    /// <summary>Optional exact topic set; mutually exclusive with Topic. Null selects all topics; an empty collection selects none.</summary>
    public IReadOnlyCollection<string>? Topics { get; init; }
    /// <summary>Optional exact topic filter.</summary>
    public string? Topic { get; init; }
    /// <summary>Inclusive log-time lower bound in caller-defined nanoseconds.</summary>
    public ulong? StartTime { get; init; }
    /// <summary>Exclusive log-time upper bound in caller-defined nanoseconds.</summary>
    public ulong? EndTime { get; init; }
    internal McapQuery ToQuery() => new() { Topic = Topic, Topics = Topics, StartTime = StartTime, EndTime = EndTime, Order = McapReadOrder.File };
}
