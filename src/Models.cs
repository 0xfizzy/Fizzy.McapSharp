namespace Fizzy.McapSharp;

public enum McapCompression
{
    None,
    Lz4,
    Zstd
}

[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
public readonly record struct McapMessageHeader(ushort ChannelId, uint Sequence, ulong LogTime, ulong PublishTime);
/// <summary>Outcome of a caller-buffer read or Sans-I/O event poll. Success describes availability, not a particular MCAP opcode.</summary>
public enum McapReadStatus
{
    /// <summary>A record, message, or protocol event is available; inspect the accompanying opcode or event kind.</summary>
    Success,
    /// <summary>The pending item is retained. Retry with a buffer at least as large as the reported required length.</summary>
    BufferTooSmall,
    /// <summary>No further items are available. This does not by itself establish full-file validation.</summary>
    EndOfStream
}

[Flags]
public enum McapRecoverableWriterErrors
{
    None = 0,
    InvalidSchemaIdOnRegistration = 1,
    ConflictingSchemaOnRegistration = 2,
    UnknownSchemaOnChannelRegistration = 4,
    ConflictingChannelOnRegistration = 8,
    UnknownChannelOnMessageWrite = 16
}

/// <summary>Writer configuration fixed at construction. Null emission/CRC switches retain upstream settings; other nullable settings have the semantics documented on their properties. Input payloads are consumed synchronously; Complete must be called explicitly.</summary>
public sealed record McapWriterOptions
{
    /// <summary>Audited errors rejected before mutation that permit continued writing; defaults to all five flags. None makes every writer error terminal. I/O and cleanup failures are never recoverable.</summary>
    public McapRecoverableWriterErrors RecoverableErrors { get; init; } =
        McapRecoverableWriterErrors.InvalidSchemaIdOnRegistration |
        McapRecoverableWriterErrors.ConflictingSchemaOnRegistration |
        McapRecoverableWriterErrors.UnknownSchemaOnChannelRegistration |
        McapRecoverableWriterErrors.ConflictingChannelOnRegistration |
        McapRecoverableWriterErrors.UnknownChannelOnMessageWrite;
    public const ulong DefaultChunkSize = 1024 * 1024;
    public McapCompression Compression { get; init; } = McapCompression.Zstd;
    /// <summary>Target uncompressed chunk bytes, default 1 MiB; null disables automatic size-based chunk flushing. Whole records may exceed the target; it is not a total-memory bound.</summary>
    public ulong? ChunkSize { get; init; } = DefaultChunkSize;
    /// <summary>Enables chunk grouping, true by default. Disabling chunks prevents chunk compression and chunk/message indexes.</summary>
    public bool UseChunks { get; init; } = true;
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

public sealed record McapSchema(ushort Id, string Name, string Encoding, byte[] Data) : IMcapParsedRecord;
public sealed record McapChannel(ushort Id, string Topic, string MessageEncoding, McapSchema? Schema, IReadOnlyDictionary<string, string> Metadata);
public sealed record McapMessage(McapChannel Channel, ulong LogTime, ulong PublishTime, uint Sequence, byte[] Data);
public sealed record McapMetadata(string Name, IReadOnlyDictionary<string, string> Values) : IMcapParsedRecord;
public sealed record McapAttachment(string Name, string MediaType, ulong LogTime, ulong CreateTime, byte[] Data);
public enum McapReadOrder { LogTime, ReverseLogTime, File }
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

/// <summary>Result of explicit prefix salvage. IsFullyValidated is true only after a strict complete scan; Error describes the failure that ended salvage, if any.</summary>
public sealed record McapRecoveryResult(ulong RecoveredMessageCount, bool IsFullyValidated, string? Error);
public sealed record McapStatistics(ulong MessageCount, ushort SchemaCount, uint ChannelCount, uint AttachmentCount, uint MetadataCount, uint ChunkCount, ulong MessageStartTime, ulong MessageEndTime, IReadOnlyDictionary<ushort, ulong> ChannelMessageCounts) : IMcapParsedRecord;
public sealed record McapChunkIndex(ulong MessageStartTime, ulong MessageEndTime, ulong ChunkStartOffset, ulong ChunkLength, IReadOnlyDictionary<ushort, ulong> MessageIndexOffsets, ulong MessageIndexLength, string Compression, ulong CompressedSize, ulong UncompressedSize) : IMcapParsedRecord;
public sealed record McapAttachmentIndex(ulong Offset, ulong Length, ulong LogTime, ulong CreateTime, ulong DataSize, string Name, string MediaType) : IMcapParsedRecord;
public sealed record McapMetadataIndex(ulong Offset, ulong Length, string Name) : IMcapParsedRecord;
public sealed record McapSummary(McapStatistics? Statistics, IReadOnlyList<McapChunkIndex> ChunkIndexes, IReadOnlyList<McapAttachmentIndex> AttachmentIndexes, IReadOnlyList<McapMetadataIndex> MetadataIndexes, IReadOnlyList<ushort> SchemaIds, IReadOnlyList<ushort> ChannelIds);
public readonly record struct McapMessageIndexEntry(ulong LogTime, ulong Offset);
public sealed record McapMessageIndex(ushort ChannelId, IReadOnlyList<McapMessageIndexEntry> Records) : IMcapParsedRecord;
public enum McapRecordMode
{
    TopLevel,
    ExpandChunks
}

/// <summary>An independently owned opcode and unparsed record body. Data excludes the opcode and length prefix. Use McapRecords.Parse for typed fields.</summary>
public sealed record McapRawRecord(byte Opcode, byte[] Data) : IMcapParsedRecord;
