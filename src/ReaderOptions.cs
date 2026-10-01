namespace Fizzy.McapSharp;

public sealed record McapReaderOptions
{
    /// <summary>Cache allowance for snapshots opened from this session. Zero disables retention; does not limit leases or upstream memory.</summary>
    public ulong MaxRandomAccessCacheBytes { get; init; }
    public bool SkipStartMagic { get; init; }
    public bool SkipEndMagic { get; init; }
    public bool CheckFinishesAfterEndMagic { get; init; }
    public bool EmitChunks { get; init; }
    public bool ValidateChunkCrcs { get; init; }
    public bool PrevalidateChunkCrcs { get; init; }
    public bool ValidateDataSectionCrc { get; init; }
    public bool ValidateSummarySectionCrc { get; init; }
    public ulong? RecordLengthLimit { get; init; }
    public static McapReaderOptions Strict { get; } = new() { PrevalidateChunkCrcs = true, ValidateDataSectionCrc = true, ValidateSummarySectionCrc = true, CheckFinishesAfterEndMagic = true };
    internal bool IsStrict => !SkipStartMagic && !SkipEndMagic && !EmitChunks && (ValidateChunkCrcs || PrevalidateChunkCrcs) && ValidateDataSectionCrc && ValidateSummarySectionCrc && CheckFinishesAfterEndMagic;
}

public sealed record McapSummaryReaderOptions
{
    public ulong? FileSize { get; init; }
    public ulong? RecordLengthLimit { get; init; }
}

public sealed partial class McapReader
{
    public McapReadSession OpenIndexedMessages(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query ?? new(), true, McapRecordMode.ExpandChunks, false, options, true);
}
