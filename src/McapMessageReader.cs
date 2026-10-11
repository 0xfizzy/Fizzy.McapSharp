namespace Fizzy.McapSharp;

/// <summary>Resolved messages with declaration-reference validation. Results follow the selected caller-buffer, borrowing or retained-storage ownership contract.</summary>
public sealed class McapMessageReader : IDisposable
{
    readonly IMessageSource core;
    internal McapMessageReader(IMessageSource core) => this.core = core;
    /// <summary>Copies complete file input into independent storage and reads messages in file order. CRC checks follow options.</summary>
    public McapMessageReader(ReadOnlySpan<byte> data, McapReaderOptions? options = null)
        => core = new ReadCursorCore(data, CursorMode.Messages, options);
    /// <summary>Maps file-order input without copying. Keep the file unchanged until the reader and all leases retaining its storage are disposed.</summary>
    public static McapMessageReader OpenMapped(string path, McapReaderOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        return new(new ReadCursorCore([], CursorMode.Messages, options, Path.GetFullPath(path)));
    }
    /// <summary>Copies the next payload into caller storage. BufferTooSmall retains the pending message; other advancement failures terminate reading.</summary>
    public McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length)
        => core.ReadNext(destination, out header, out length);
    /// <summary>Visits one payload synchronously. The span expires on return and reader reentry is prohibited. Callback failures terminate reading.</summary>
    public McapReadStatus ReadNext(McapMessageVisitor visitor) => core.ReadNext(visitor);
    /// <summary>Visits a bounded number of messages synchronously without reader reentry. Failure returns no prefix count and terminates reading.</summary>
    public McapVisitResult VisitMessages(McapMessageVisitor visitor, int maxMessages = 256) => core.VisitMessages(visitor, maxMessages);
    /// <summary>Copies a batch into caller storage. BufferTooSmall preserves a pending message and reports a valid prefix; discard all output if an exception is thrown.</summary>
    public McapBatchReadResult ReadBatch(Span<McapMessageHeader> headers, Span<McapPayloadRange> ranges, Span<byte> payloadStorage)
        => core.ReadBatch(headers, ranges, payloadStorage);
    /// <summary>Returns retained message storage, or null at EOF. Leases survive advancement and reader disposal. Failure publishes no partial lease and terminates reading.</summary>
    public McapMessageBatchLease? ReadBatchLease(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024)
        => core.ReadBatchLease(maxMessages, targetPayloadBytes);
    /// <summary>Copies an encountered or snapshot-provided channel and schema. Unknown IDs do not advance or terminate reading.</summary>
    public McapChannel GetChannel(ushort id) => core.GetChannel(id);
    /// <summary>Returns a cached immutable encountered or snapshot-provided channel. The description survives reader disposal; failed readers reject lookup.</summary>
    public McapChannelDescription GetChannelDescription(ushort id) => core.GetChannelDescription(id);
    /// <summary>Releases parser state before input. Independently retained leases and owned results remain valid.</summary>
    public void Dispose() => core.Dispose();
    /// <summary>Copies the message wire body including its 22-byte header. Shares pending state with payload, callback, batch and lease delivery.</summary>
    public McapReadStatus ReadNextMessageRecord(Span<byte> destination, out ulong length)
        => core.ReadNextMessageRecord(destination, out length);
    /// <summary>Advances this reader and yields independent mutable messages, including copied payloads and declarations.</summary>
    public IEnumerable<McapMessage> ReadMessages() => core.ReadMessages();
    /// <summary>True after this scan reaches EOF without failure; this alone does not prove full-file validation.</summary>
    public bool IsScanComplete => ((IReadProgress)core).IsScanComplete;
    /// <summary>True only after a strict complete expanded scan. Indexed, chunk-local and raw-message reads cannot prove full-file validation.</summary>
    public bool IsFullyValidated => ((IReadProgress)core).IsFullyValidated;
    /// <summary>Native record count from a completed scan, not a live progress counter.</summary>
    public ulong ScannedRecordCount => ((IReadProgress)core).ScannedRecordCount;
    /// <summary>Consumes a strict complete expanded scan and returns its record count. Partial or non-strict capabilities cannot establish full-file validation.</summary>
    public ulong ValidateRemaining() => ((IReadProgress)core).ValidateRemaining();
    /// <summary>Releases native state and transfers the underlying Stream. Copied, mapped and path input have no managed Stream to transfer.</summary>
    public Stream IntoInner() => ((IReadProgress)core).IntoInner();
    /// <summary>Copies an encountered or retained summary schema without advancing. Unknown IDs do not terminate reading.</summary>
    public McapSchema GetSchema(ushort id) => ((IDeclarationSource)core).GetSchema(id);
    /// <summary>Returns a cached immutable encountered or retained summary schema. The description survives reader disposal.</summary>
    public McapSchemaDescription GetSchemaDescription(ushort id) => ((IDeclarationSource)core).GetSchemaDescription(id);
    /// <summary>Delivers a strict valid prefix. Parse errors are retained in the result; callback exceptions propagate and terminate this reader. Indexed or non-strict reading cannot establish validated prefix recovery.</summary>
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept) => core.RecoverMessages(accept);
}
