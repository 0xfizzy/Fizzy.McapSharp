using System.Runtime.InteropServices;
namespace Fizzy.McapSharp;
internal sealed partial class ReadCursorCore
{
    (bool Complete, ulong Count)? finalProgress;
    (bool Complete, ulong Count) Progress()
    {
        lock (gate)
        {
            borrowed.CheckReentry();
            if (finalProgress is { } final) return final;
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            if (failed) return (false, 0);
            int status = Native.fm_buffer_reader_state(handle, out var result);
            if (status < Protocol.Status.Success) throw Native.ConsumeError(result);
            return (status == Protocol.Status.End, result.Value);
        }
    }
    public bool IsScanComplete => Progress().Complete;
    public bool IsFullyValidated => Progress().Complete && fullyValidating;
    public ulong ScannedRecordCount => Progress().Count;
    public ulong ValidateRemaining()
    {
        lock (gate) Check();
        if (!fullyValidating) throw new InvalidOperationException("Full validation requires a strict complete expanded scan.");
        byte[] buffer = new byte[65536];
        while (true)
        {
            var status = ReadNextRecord(buffer, out _, out var length);
            if (status == McapReadStatus.EndOfStream) return ScannedRecordCount;
            if (status == McapReadStatus.BufferTooSmall) buffer = new byte[checked((int)length)];
        }
    }
    public Stream IntoInner()
    {
        lock (gate) Check();
        throw new NotSupportedException("Copied or mapped input has no managed Stream to transfer.");
    }
    public McapRecoveryResult RecoverMessages(Action<McapMessage> accept)
    {
        ArgumentNullException.ThrowIfNull(accept);
        lock (gate) CheckMessages();
        if (!fullyValidating) throw new InvalidOperationException("Prefix recovery requires a strict complete expanded scan.");
        ulong count = 0;
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.MessageBody);
        while (true)
        {
            bool available;
            try { available = ReadOwned(sink); }
            catch (McapException error) when (error.Kind is not (McapErrorKind.Io or McapErrorKind.Binding))
            { return new(count, false, error); }
            if (!available) return new(count, IsFullyValidated, null);
            var h = sink.Header;
            try { accept(new(GetChannel(h.ChannelId), h.LogTime, h.PublishTime, h.Sequence, (byte[])sink.Value!)); }
            catch { lock (gate) failed = true; throw; }
            count++;
        }
    }

}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_state(BufferReaderHandle reader, out Result result);
}
