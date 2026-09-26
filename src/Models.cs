namespace Fizzy.McapSharp;

public sealed class McapException(string message) : IOException(message);
public enum McapCompression { None, Lz4, Zstd }
public sealed record McapWriterOptions
{
    public McapCompression Compression { get; init; }
    public ulong ChunkSize { get; init; } = 4 * 1024 * 1024;
    public bool UseChunks { get; init; } = true;
    public bool EmitIndexes { get; init; } = true;
    public string Profile { get; init; } = "";
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
