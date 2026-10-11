namespace Fizzy.McapSharp;

internal interface IRecordSource : IDisposable
{
    McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length);
    IEnumerable<McapRawRecord> ReadRecords();
}
internal interface IMessageSource : IDisposable
{
    McapRecoveryResult RecoverMessages(Action<McapMessage> accept);
    McapReadStatus ReadNextMessageRecord(Span<byte> destination, out ulong length);
    McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length);
    McapReadStatus ReadNext(McapMessageVisitor visitor);
    McapVisitResult VisitMessages(McapMessageVisitor visitor, int maxMessages);
    McapBatchReadResult ReadBatch(Span<McapMessageHeader> headers, Span<McapPayloadRange> ranges, Span<byte> payloadStorage);
    McapMessageBatchLease? ReadBatchLease(int maxMessages, int targetPayloadBytes);
    McapChannel GetChannel(ushort id);
    McapChannelDescription GetChannelDescription(ushort id);
    IEnumerable<McapMessage> ReadMessages();
}

internal interface IReadProgress
{
    McapValidationCapability ValidationCapability { get; }
    bool IsScanComplete { get; }
    bool IsFullyValidated { get; }
    ulong ScannedRecordCount { get; }
    ulong ValidateRemaining();
    Stream IntoInner();
}

internal interface IDeclarationSource
{
    McapSchema GetSchema(ushort id);
    McapChannel GetChannel(ushort id);
    McapSchemaDescription GetSchemaDescription(ushort id);
    McapChannelDescription GetChannelDescription(ushort id);
}
