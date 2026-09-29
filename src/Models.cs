namespace Fizzy.McapSharp;
public sealed class McapException(string message) : IOException(message);
public enum McapCompression
{
    None,
    Lz4,
    Zstd
}

public readonly record struct McapMessageHeader(ushort ChannelId, uint Sequence, ulong LogTime, ulong PublishTime);
public enum McapReadStatus
{
    Message,
    BufferTooSmall,
    EndOfStream
}

public sealed record McapWriterOptions
{
    public McapCompression Compression { get; init; } = McapCompression.None;
    public ulong? ChunkSize { get; init; } = 4 * 1024 * 1024;
    public bool UseChunks { get; init; } = true;
    public string Profile { get; init; } = "";
    public string? Library { get; init; }
    public bool? EmitSummaryOffsets { get; init; } = true;
    public bool? EmitStatistics { get; init; } = true;
    public bool? EmitMessageIndexes { get; init; } = true;
    public bool? EmitChunkIndexes { get; init; } = true;
    public bool? EmitAttachmentIndexes { get; init; } = true;
    public bool? EmitMetadataIndexes { get; init; } = true;
    public bool? RepeatChannels { get; init; }
    public bool? RepeatSchemas { get; init; }
    public bool? CalculateChunkCrcs { get; init; } = true;
    public bool? CalculateDataSectionCrc { get; init; } = true;
    public bool? CalculateSummarySectionCrc { get; init; } = true;
    public bool? CalculateAttachmentCrcs { get; init; } = true;
    public uint? CompressionLevel { get; init; }
    public uint? CompressionThreads { get; init; }
}

public sealed record McapSchema(ushort Id, string Name, string Encoding, byte[] Data);
public sealed record McapChannel(ushort Id, string Topic, string MessageEncoding, McapSchema? Schema, IReadOnlyDictionary<string, string> Metadata);
public sealed record McapMessage(McapChannel Channel, ulong LogTime, ulong PublishTime, uint Sequence, byte[] Data);
public sealed record McapMetadata(string Name, IReadOnlyDictionary<string, string> Values);
public sealed record McapAttachment(string Name, string MediaType, ulong LogTime, ulong CreateTime, byte[] Data);
public sealed record McapQuery
{
    public string? Topic { get; init; }
    public ulong? StartTime { get; init; }
    public ulong? EndTime { get; init; }
}

public sealed record McapRecoveryResult(ulong RecoveredMessageCount, bool IsComplete, string? Error);
public sealed record McapStatistics(ulong MessageCount, ushort SchemaCount, uint ChannelCount, uint AttachmentCount, uint MetadataCount, uint ChunkCount, ulong MessageStartTime, ulong MessageEndTime, IReadOnlyDictionary<ushort, ulong> ChannelMessageCounts);
public sealed record McapChunkIndex(ulong MessageStartTime, ulong MessageEndTime, ulong ChunkStartOffset, ulong ChunkLength, IReadOnlyDictionary<ushort, ulong> MessageIndexOffsets, ulong MessageIndexLength, string Compression, ulong CompressedSize, ulong UncompressedSize);
public sealed record McapAttachmentIndex(ulong Offset, ulong Length, ulong LogTime, ulong CreateTime, ulong DataSize, string Name, string MediaType);
public sealed record McapMetadataIndex(ulong Offset, ulong Length, string Name);
public sealed record McapSummary(McapStatistics? Statistics, IReadOnlyList<McapChunkIndex> ChunkIndexes, IReadOnlyList<McapAttachmentIndex> AttachmentIndexes, IReadOnlyList<McapMetadataIndex> MetadataIndexes, IReadOnlyList<ushort> SchemaIds, IReadOnlyList<ushort> ChannelIds);
public readonly record struct McapMessageIndexEntry(ulong LogTime, ulong Offset);
public sealed record McapMessageIndex(ushort ChannelId, IReadOnlyList<McapMessageIndexEntry> Records);
public enum McapRecordMode
{
    TopLevel,
    ExpandChunks
}

public sealed record McapRecord(byte Opcode, byte[] Data);
