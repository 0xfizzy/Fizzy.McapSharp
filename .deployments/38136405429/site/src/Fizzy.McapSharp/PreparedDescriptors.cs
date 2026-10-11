namespace Fizzy.McapSharp;

/// <summary>Copies schema fields and bytes for repeated registration. Null ID requests allocation; explicit ID must be nonzero. Preparation does not write or register anything.</summary>
public sealed class McapPreparedSchema : IDisposable
{
    internal readonly PreparedWrite Value;
    /// <summary>Copies schema fields and bytes for repeated registration. Null ID requests allocation; explicit ID must be nonzero.</summary>
    public McapPreparedSchema(string name, string encoding, ReadOnlySpan<byte> data, ushort? id = null) => Value = PreparedWrite.Schema(name, encoding, data, id);
    /// <summary>Releases the descriptor once. Do not dispose concurrently with a writer operation using it.</summary>
    public void Dispose() => Value.Dispose();
}

/// <summary>Copies a channel registration descriptor referring to a schema ID. This is distinct from a resolved prepared channel. Preparation does not write or register anything.</summary>
public sealed class McapPreparedChannelRegistration : IDisposable
{
    internal readonly PreparedWrite Value;
    /// <summary>Copies a channel registration descriptor referring to a schema ID. This is distinct from a resolved prepared channel.</summary>
    public McapPreparedChannelRegistration(string topic, string encoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null, ushort? id = null) => Value = PreparedWrite.Channel(topic, encoding, schemaId, metadata, id);
    /// <summary>Releases the descriptor once. Do not dispose concurrently with a writer operation using it.</summary>
    public void Dispose() => Value.Dispose();
}

/// <summary>Copies named metadata for repeated synchronous writes. Preparation does not write or register anything.</summary>
public sealed class McapPreparedMetadata : IDisposable
{
    internal readonly PreparedWrite Value;
    /// <summary>Copies named metadata for repeated synchronous writes.</summary>
    public McapPreparedMetadata(string name, IReadOnlyDictionary<string, string> metadata) => Value = PreparedWrite.Metadata(name, metadata);
    /// <summary>Releases the descriptor once. Do not dispose concurrently with a writer operation using it.</summary>
    public void Dispose() => Value.Dispose();
}

/// <summary>Copies attachment fields. The payload is supplied separately at each write. Preparation does not write or register anything.</summary>
public sealed class McapPreparedAttachment : IDisposable
{
    internal readonly PreparedWrite Value;
    /// <summary>Copies attachment fields. The payload is supplied separately at each write.</summary>
    public McapPreparedAttachment(string name, string mediaType, ulong logTime, ulong createTime) => Value = PreparedWrite.Attachment(name, mediaType, logTime, createTime);
    /// <summary>Releases the descriptor once. Do not dispose concurrently with a writer operation using it.</summary>
    public void Dispose() => Value.Dispose();
}

/// <summary>Copies segmented attachment fields and the exact declared byte length. Preparation does not write or register anything.</summary>
public sealed class McapPreparedAttachmentStart : IDisposable
{
    internal readonly PreparedWrite Value;
    /// <summary>Copies segmented attachment fields and the exact declared byte length.</summary>
    public McapPreparedAttachmentStart(string name, string mediaType, ulong logTime, ulong createTime, ulong length) => Value = PreparedWrite.StartAttachment(name, mediaType, logTime, createTime, length);
    /// <summary>Releases the descriptor once. Do not dispose concurrently with a writer operation using it.</summary>
    public void Dispose() => Value.Dispose();
}
