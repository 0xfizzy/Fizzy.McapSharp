using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Immutable owned descriptor prepared once for repeated control writes. Factories copy descriptor values and schema bytes; preparation does not register or write anything. Dispose after the last synchronous use.</summary>
public sealed class McapPreparedOperation : IDisposable
{
    internal readonly OperationHandle Handle;
    internal readonly uint Operation;
    unsafe McapPreparedOperation(uint operation, object args, ReadOnlySpan<byte> data = default)
    {
        Native.EnsureAvailable(); Operation = operation;
        var req = Native.Request(args);
        fixed (byte* p = data)
        {
            int status = Native.fm_operation_prepare(operation, req, (nuint)req.Length, p, (nuint)data.Length, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose(); Handle = new(h);
        }
    }
    /// <summary>Copies schema fields and bytes for repeated registration. Null ID requests automatic allocation; an explicit ID must be nonzero.</summary>
    public static McapPreparedOperation Schema(string name, string encoding, ReadOnlySpan<byte> data, ushort? id = null)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(encoding);
        return new(Protocol.WriterOperation.Schema, new { name, encoding, id }, data);
    }
    /// <summary>Copies channel fields for repeated registration. Schema ID zero means no schema; null metadata means empty metadata.</summary>
    public static McapPreparedOperation Channel(string topic, string encoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null, ushort? id = null)
    {
        ArgumentNullException.ThrowIfNull(encoding);
        ArgumentNullException.ThrowIfNull(topic);
        if (metadata is not null) ArgumentValidation.ValidateMetadata(metadata);
        return new(Protocol.WriterOperation.Channel, new { topic, encoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>(), id });
    }
    /// <summary>Copies metadata fields for repeated writes. Name, dictionary, keys and values must be non-null.</summary>
    public static McapPreparedOperation Metadata(string name, IReadOnlyDictionary<string, string> metadata)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentValidation.ValidateMetadata(metadata);
        return new(Protocol.WriterOperation.Metadata, new { name, metadata });
    }
    /// <summary>Prepares attachment fields; supply each complete payload to WritePrepared. Times use caller-defined nanoseconds.</summary>
    public static McapPreparedOperation Attachment(string name, string mediaType, ulong logTime, ulong createTime)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(mediaType);
        return new(Protocol.WriterOperation.Attachment, new { name, media_type = mediaType, log_time = logTime, create_time = createTime });
    }
    /// <summary>Prepares a segmented attachment of the exact byte length. WritePrepared accepts no payload; follow with WriteAttachmentBytes and FinishAttachment.</summary>
    public static McapPreparedOperation StartAttachment(string name, string mediaType, ulong logTime, ulong createTime, ulong length)
    {
        ArgumentNullException.ThrowIfNull(name);
        ArgumentNullException.ThrowIfNull(mediaType);
        return new(Protocol.WriterOperation.StartAttachment, new { name, media_type = mediaType, log_time = logTime, create_time = createTime, length });
    }
    /// <summary>Releases this descriptor after the last synchronous use. Do not dispose concurrently with a write.</summary>
    public void Dispose() => Handle.Dispose();
}
