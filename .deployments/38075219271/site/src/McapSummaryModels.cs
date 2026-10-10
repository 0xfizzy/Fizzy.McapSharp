namespace Fizzy.McapSharp;

/// <summary>Owned summary statistics. Message times are nanoseconds; counts describe the recorded data section.</summary>
public sealed record McapStatistics(ulong MessageCount, ushort SchemaCount, uint ChannelCount, uint AttachmentCount, uint MetadataCount, uint ChunkCount, ulong MessageStartTime, ulong MessageEndTime, IReadOnlyDictionary<ushort, ulong> ChannelMessageCounts) : IMcapRecord;

/// <summary>Owned chunk index. ChunkStartOffset and MessageIndexOffsets are byte offsets from the MCAP origin (the initial Stream position for Stream inputs); lengths and sizes are bytes; times are nanoseconds.</summary>
public sealed record McapChunkIndex(ulong MessageStartTime, ulong MessageEndTime, ulong ChunkStartOffset, ulong ChunkLength, IReadOnlyDictionary<ushort, ulong> MessageIndexOffsets, ulong MessageIndexLength, string Compression, ulong CompressedSize, ulong UncompressedSize) : IMcapRecord;

/// <summary>Owned attachment index with byte Offset from the MCAP origin (the initial Stream position for Stream inputs) and record Length; DataSize counts payload bytes and times are nanoseconds.</summary>
public sealed record McapAttachmentIndex(ulong Offset, ulong Length, ulong LogTime, ulong CreateTime, ulong DataSize, string Name, string MediaType) : IMcapRecord;

/// <summary>Owned metadata index with byte Offset from the MCAP origin (the initial Stream position for Stream inputs) and complete record Length in bytes.</summary>
public sealed record McapMetadataIndex(ulong Offset, ulong Length, string Name) : IMcapRecord;

/// <summary>Summary model. Library-returned snapshots own independent collections; direct construction retains supplied references. Size grows with file indexes and declarations.</summary>
public sealed record McapSummary(McapStatistics? Statistics, IReadOnlyList<McapChunkIndex> ChunkIndexes, IReadOnlyList<McapAttachmentIndex> AttachmentIndexes, IReadOnlyList<McapMetadataIndex> MetadataIndexes, IReadOnlyList<ushort> SchemaIds, IReadOnlyList<ushort> ChannelIds);

/// <summary>Message log time in nanoseconds and byte offset within the uncompressed chunk record stream, not an offset from the MCAP origin.</summary>
public readonly record struct McapMessageIndexEntry(ulong LogTime, ulong Offset);

/// <summary>Owned per-channel message index, including channels with an empty Records collection.</summary>
public sealed record McapMessageIndex(ushort ChannelId, IReadOnlyList<McapMessageIndexEntry> Records) : IMcapRecord;
