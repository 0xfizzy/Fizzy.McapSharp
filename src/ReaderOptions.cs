namespace Fizzy.McapSharp;

/// <summary>Parsing and integrity options for one read session. Defaults do not prove full-file integrity; a strict complete expanded synchronous scan provides full-file validation. High-level body and declaration checks do not depend on this preset.</summary>
public sealed record McapReaderOptions
{
    /// <summary>Skips the leading magic; false by default. Enabling this prevents full-file validation.</summary>
    public bool SkipStartMagic { get; init; }
    /// <summary>Skips the trailing magic; false by default. Enabling this prevents full-file validation.</summary>
    public bool SkipEndMagic { get; init; }
    /// <summary>Rejects bytes following the trailing magic; false by default, true in Strict.</summary>
    public bool CheckFinishesAfterEndMagic { get; init; }
    /// <summary>Emits encoded chunks in asynchronous record or direct linear Sans-I/O reading. False by default; chunk flatteners and message readers reject true. Synchronous top-level reading is selected with McapLinearReader.</summary>
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
    /// <summary>Validates chunk, data and summary CRCs, required magic, and trailing-byte absence. All high-level expanded readers validate record bodies and declaration references independently of these switches. Direct Sans-I/O exposes raw events without body parsing. Indexed query success does not establish full-file validation.</summary>
    public static McapReaderOptions Strict { get; } = new() { PrevalidateChunkCrcs = true, ValidateDataSectionCrc = true, ValidateSummarySectionCrc = true, CheckFinishesAfterEndMagic = true };
    internal bool IsStrict => !SkipStartMagic && !SkipEndMagic && !EmitChunks && (ValidateChunkCrcs || PrevalidateChunkCrcs) && ValidateDataSectionCrc && ValidateSummarySectionCrc && CheckFinishesAfterEndMagic;
}

/// <summary>Options for direct Sans-I/O summary reading. FileSize permits end-relative seeks to be resolved against the caller-provided source length.</summary>
public sealed record McapSummaryReaderOptions
{
    /// <summary>Total source length in bytes, if known; null leaves end-relative seek resolution to the caller.</summary>
    public ulong? FileSize { get; init; }
    /// <summary>Maximum accepted record body length in bytes; null leaves the upstream limit unset. This is not a bound on total parser, cache or lease memory.</summary>
    public ulong? RecordLengthLimit { get; init; }
}
