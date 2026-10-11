namespace Fizzy.McapSharp;

public sealed partial class McapWriter
{
    /// <summary>Registers a prepared schema using its captured fields and bytes, returning its ID. Ordinary writer failure and configured safe-rejection rules apply.</summary>
    public ushort RegisterSchema(McapPreparedSchema prepared)
    {
        ArgumentNullException.ThrowIfNull(prepared);
        return ExecutePrepared(prepared.Value)!.Value;
    }
    /// <summary>Registers a prepared channel referring to a known schema ID, returning its ID. Ordinary writer failure and configured safe-rejection rules apply.</summary>
    public ushort RegisterChannel(McapPreparedChannelRegistration prepared)
    {
        ArgumentNullException.ThrowIfNull(prepared);
        return ExecutePrepared(prepared.Value)!.Value;
    }
    /// <summary>Writes captured metadata synchronously. Ordinary writer failure rules apply.</summary>
    public void WriteMetadata(McapPreparedMetadata prepared)
    {
        ArgumentNullException.ThrowIfNull(prepared);
        ExecutePrepared(prepared.Value);
    }
    /// <summary>Writes captured attachment fields and synchronously consumes the supplied payload. Ordinary writer failure rules apply.</summary>
    public void WriteAttachment(McapPreparedAttachment prepared, ReadOnlySpan<byte> payload = default)
    {
        ArgumentNullException.ThrowIfNull(prepared);
        ExecutePrepared(prepared.Value, payload);
    }
    /// <summary>Starts a segmented attachment using captured fields and declared length. Follow with WriteAttachmentBytes and FinishAttachment.</summary>
    public void StartAttachment(McapPreparedAttachmentStart prepared)
    {
        ArgumentNullException.ThrowIfNull(prepared);
        ExecutePrepared(prepared.Value);
    }
}
