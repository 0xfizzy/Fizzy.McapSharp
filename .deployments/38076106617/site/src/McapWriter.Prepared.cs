namespace Fizzy.McapSharp;

public sealed partial class McapWriter
{
    /// <summary>Writes an owned message with automatic declarations, observing current metadata and schema bytes. Preparation may allocate.</summary>
    public void WriteMessage(McapMessage message)
    {
        ArgumentNullException.ThrowIfNull(message);
        ArgumentNullException.ThrowIfNull(message.Data);
        using var channel = new McapPreparedChannel(message.Channel);
        WriteMessage(channel, new(message.Channel.Id, message.Sequence, message.LogTime, message.PublishTime), message.Data);
    }

    /// <summary>Writes with automatic declarations from an immutable prepared channel. Header IDs must match; payload is consumed synchronously and warmed writes allocate zero managed bytes.</summary>
    public unsafe void WriteMessage(McapPreparedChannel channel, in McapMessageHeader header, ReadOnlySpan<byte> data)
    {
        ArgumentNullException.ThrowIfNull(channel);
        if (header.ChannelId != channel.Id) throw new ArgumentException("Header and prepared channel IDs differ.", nameof(header));
        lock (gate)
        {
            Check();
            ObjectDisposedException.ThrowIf(channel.Handle.IsClosed, channel);
            var h = new Native.NativeHeader { ChannelId = header.ChannelId, Sequence = header.Sequence, LogTime = header.LogTime, PublishTime = header.PublishTime };
            bool safeRejection = false;
            try
            {
                fixed (byte* p = data)
                {
                    var status = Native.fm_writer_full_message(handle, channel.Handle, &h, p, (nuint)data.Length, out var r);
                    CheckResult(status, r, ref safeRejection);
                }
            }
            catch { if (!safeRejection) failed = true; throw; }
        }
    }

}

public sealed partial class McapWriter
{
    /// <summary>Runs the prepared operation synchronously; returns the registered ID for schema/channel operations, otherwise null. Channel ID zero is a successful registration result. Payload is consumed only by Attachment. Schema uses the bytes copied when prepared; other operations reject nonempty payload before invoking native code. StartAttachment declares the length but consumes no body; follow with WriteAttachmentBytes and FinishAttachment. Writer failure and audited rejection rules still apply.</summary>
    public unsafe ushort? WritePrepared(McapPreparedOperation operation, ReadOnlySpan<byte> payload = default)
    {
        ArgumentNullException.ThrowIfNull(operation);
        if (operation.Operation != Protocol.WriterOperation.Attachment && !payload.IsEmpty)
            throw new ArgumentException("Only prepared Attachment operations accept a payload.", nameof(payload));
        lock (gate)
        {
            Check(); ObjectDisposedException.ThrowIf(operation.Handle.IsClosed, operation);
            bool safeRejection = false;
            try
            {
                fixed (byte* p = payload)
                {
                    int status = Native.fm_writer_prepared(handle, operation.Handle, p, (nuint)payload.Length, out var r);
                    CheckResult(status, r, ref safeRejection);
                    if (operation.Operation == Protocol.WriterOperation.StartAttachment) attachment = true;
                    return operation.Operation is Protocol.WriterOperation.Schema or Protocol.WriterOperation.Channel
                        ? checked((ushort)r.Value) : null;
                }
            }
            catch { if (!safeRejection) failed = true; throw; }
        }
    }
}
