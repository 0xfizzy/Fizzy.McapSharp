namespace Fizzy.McapSharp;

/// <summary>Parsing and integrity options for one read session. Defaults do not prove full-file integrity; use Strict and consume the complete expanded scan for validation.</summary>
public sealed record McapReaderOptions
{
    /// <summary>Local cache allowance for snapshots opened from this session: chunk storage, descriptors, keys and index bytes. Zero disables retention; oversized entries load without retention. Excludes input storage, parsing temporaries and external leases.</summary>
    public ulong MaxRandomAccessCacheBytes { get; init; }
    /// <summary>Skips the leading magic; false by default. Enabling this prevents full-file validation.</summary>
    public bool SkipStartMagic { get; init; }
    /// <summary>Skips the trailing magic; false by default. Enabling this prevents full-file validation.</summary>
    public bool SkipEndMagic { get; init; }
    /// <summary>Rejects bytes following the trailing magic; false by default, true in Strict.</summary>
    public bool CheckFinishesAfterEndMagic { get; init; }
    /// <summary>Emits chunk records without expanding their contained records; false by default. Incompatible with asynchronous message leases.</summary>
    public bool EmitChunks { get; init; }
    /// <summary>Checks chunk CRCs while parsing; false by default. A failure may occur after messages from the chunk were delivered.</summary>
    public bool ValidateChunkCrcs { get; init; }
    /// <summary>Checks a chunk CRC before delivering its contents; false by default, true in Strict.</summary>
    public bool PrevalidateChunkCrcs { get; init; }
    /// <summary>Checks the data-section CRC at its end; false by default. Earlier results do not establish validation success.</summary>
    public bool ValidateDataSectionCrc { get; init; }
    /// <summary>Checks the summary CRC; false by default, true in Strict.</summary>
    public bool ValidateSummarySectionCrc { get; init; }
    /// <summary>Maximum accepted record body length in bytes; null leaves the upstream limit unset. This is not a bound on total parser, cache or lease memory.</summary>
    public ulong? RecordLengthLimit { get; init; }
    /// <summary>Validates chunk, data and summary CRCs, required magic, and the absence of trailing bytes. Indexed query success still does not establish full-file validation.</summary>
    public static McapReaderOptions Strict { get; } = new() { PrevalidateChunkCrcs = true, ValidateDataSectionCrc = true, ValidateSummarySectionCrc = true, CheckFinishesAfterEndMagic = true };
    internal bool IsStrict => !SkipStartMagic && !SkipEndMagic && !EmitChunks && (ValidateChunkCrcs || PrevalidateChunkCrcs) && ValidateDataSectionCrc && ValidateSummarySectionCrc && CheckFinishesAfterEndMagic;
}

public sealed record McapSummaryReaderOptions
{
    public ulong? FileSize { get; init; }
    /// <summary>Maximum accepted record body length in bytes; null leaves the upstream limit unset. This is not a bound on total parser, cache or lease memory.</summary>
    public ulong? RecordLengthLimit { get; init; }
}

public sealed partial class McapFileReader
{
    public McapReadSession OpenIndexedMessages(McapQuery? query = null, McapReaderOptions? options = null) => new(path, null, query ?? new(), true, McapRecordMode.ExpandChunks, false, options, true);
}
