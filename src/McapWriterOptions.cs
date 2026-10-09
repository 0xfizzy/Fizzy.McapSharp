namespace Fizzy.McapSharp;

/// <summary>Compression codec used for chunks; None leaves record bytes uncompressed.</summary>
public enum McapCompression
{
    /// <summary>Leaves chunk payloads uncompressed.</summary>
    None,
    /// <summary>Compresses chunks with LZ4.</summary>
    Lz4,
    /// <summary>Compresses chunks with Zstandard.</summary>
    Zstd
}

/// <summary>Audited pre-mutation native rejections that may leave the writer usable. I/O and cleanup failures remain terminal.</summary>
[Flags]
public enum McapWriterSafeRejections
{
    /// <summary>Treats every native writer rejection as terminal.</summary>
    None = 0,
    /// <summary>Allows rejecting schema ID zero without terminating the writer.</summary>
    InvalidSchemaIdOnRegistration = 1,
    /// <summary>Allows rejecting conflicting schema content for an existing ID.</summary>
    ConflictingSchemaOnRegistration = 2,
    /// <summary>Allows rejecting a channel referring to an unknown schema.</summary>
    UnknownSchemaOnChannelRegistration = 4,
    /// <summary>Allows rejecting conflicting channel content for an existing ID.</summary>
    ConflictingChannelOnRegistration = 8,
    /// <summary>Allows rejecting a message referring to an unknown channel.</summary>
    UnknownChannelOnMessageWrite = 16
}

/// <summary>Writer configuration fixed at construction. Null emission/CRC switches retain upstream settings; other nullable settings have the semantics documented on their properties. Input payloads are consumed synchronously; Complete must be called explicitly.</summary>
public sealed record McapWriterOptions
{
    /// <summary>Audited errors rejected before mutation that permit continued writing; defaults to all five flags. None makes every writer error terminal. I/O and cleanup failures are never recoverable.</summary>
    public McapWriterSafeRejections SafeRejections { get; init; } =
        McapWriterSafeRejections.InvalidSchemaIdOnRegistration |
        McapWriterSafeRejections.ConflictingSchemaOnRegistration |
        McapWriterSafeRejections.UnknownSchemaOnChannelRegistration |
        McapWriterSafeRejections.ConflictingChannelOnRegistration |
        McapWriterSafeRejections.UnknownChannelOnMessageWrite;
    /// <summary>Default target uncompressed chunk size in bytes, not a hard size or memory limit.</summary>
    public const ulong DefaultChunkSize = 1024 * 1024;
    /// <summary>Chunk compression codec, Zstd by default. Has no effect when chunks are disabled.</summary>
    public McapCompression Compression { get; init; } = McapCompression.Zstd;
    /// <summary>Target uncompressed chunk bytes, default 1 MiB; null disables automatic size-based chunk flushing. Whole records may exceed the target; it is not a total-memory bound.</summary>
    public ulong? ChunkSize { get; init; } = DefaultChunkSize;
    /// <summary>Enables chunk grouping, true by default. Disabling chunks prevents chunk compression and chunk/message indexes.</summary>
    public bool UseChunks { get; init; } = true;
    /// <summary>File profile identifier, empty by default; must be non-null.</summary>
    public string Profile { get; init; } = "";
    /// <summary>Header library identifier; null uses the bundled Rust MCAP identifier.</summary>
    public string? Library { get; init; }
    /// <summary>Null chooses based on output seekability. True uses buffered chunk writing; false requires a seekable output.</summary>
    public bool? DisableSeeking { get; init; }
    /// <summary>Sets statistics, summary indexes and repeated declarations together. Individual non-null flags below override it. Does not control summary-offset or message-index records; null retains upstream defaults.</summary>
    public bool? EmitSummaryRecords { get; init; }
    /// <summary>Emits summary-group location records; null retains the upstream default (enabled). Independent of EmitSummaryRecords.</summary>
    public bool? EmitSummaryOffsets { get; init; }
    /// <summary>Emits summary statistics. Null keeps the setting selected by EmitSummaryRecords, or the upstream default (enabled).</summary>
    public bool? EmitStatistics { get; init; }
    /// <summary>Emits per-channel message indexes after chunks. Null retains the upstream default (enabled); has no effect without chunks.</summary>
    public bool? EmitMessageIndexes { get; init; }
    /// <summary>Emits chunk indexes in the summary. Null keeps EmitSummaryRecords or the upstream default (enabled).</summary>
    public bool? EmitChunkIndexes { get; init; }
    /// <summary>Emits attachment indexes in the summary. Null keeps EmitSummaryRecords or the upstream default (enabled).</summary>
    public bool? EmitAttachmentIndexes { get; init; }
    /// <summary>Emits metadata indexes in the summary. Null keeps EmitSummaryRecords or the upstream default (enabled).</summary>
    public bool? EmitMetadataIndexes { get; init; }
    /// <summary>Repeats channel declarations in the summary. Null keeps EmitSummaryRecords or the upstream default (enabled).</summary>
    public bool? RepeatChannels { get; init; }
    /// <summary>Repeats schema declarations in the summary. Null keeps EmitSummaryRecords or the upstream default (enabled).</summary>
    public bool? RepeatSchemas { get; init; }
    /// <summary>Calculates uncompressed chunk CRCs, enabled by the binding default. False omits the checksum; null retains the upstream default.</summary>
    public bool? CalculateChunkCrcs { get; init; } = true;
    /// <summary>Calculates the data-section CRC, enabled by the binding default. False omits the checksum; null retains the upstream default.</summary>
    public bool? CalculateDataSectionCrc { get; init; } = true;
    /// <summary>Calculates the summary-section CRC, enabled by the binding default. False omits the checksum; null retains the upstream default.</summary>
    public bool? CalculateSummarySectionCrc { get; init; } = true;
    /// <summary>Calculates attachment CRCs, enabled by the binding default. False omits the checksum; null retains the upstream default.</summary>
    public bool? CalculateAttachmentCrcs { get; init; } = true;
    /// <summary>Codec-specific compression level; zero chooses the codec default and null retains upstream configuration.</summary>
    public uint? CompressionLevel { get; init; }
    /// <summary>Zstd worker count; zero disables multithreaded compression. Null uses the upstream physical-CPU-count default. Does not control reader or binding threads.</summary>
    public uint? CompressionThreads { get; init; }
}
