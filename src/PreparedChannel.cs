using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>An immutable native snapshot. Preparing does not register or write declarations.</summary>
public sealed class McapPreparedChannel : IDisposable
{
    internal readonly PreparedChannelHandle Handle;
    public ushort Id { get; }
    public unsafe McapPreparedChannel(McapChannel channel)
    {
        ArgumentNullException.ThrowIfNull(channel);
        Native.EnsureAvailable();
        Id = channel.Id;
        var s = channel.Schema;
        var req = Native.Request(new
        {
            id = channel.Id,
            topic = channel.Topic,
            encoding = channel.MessageEncoding,
            metadata = channel.Metadata,
            schema = s is null ? null : new { id = s.Id, name = s.Name, encoding = s.Encoding }
        });
        fixed (byte* data = s?.Data)
        {
            int status = Native.fm_channel_prepare(req, (nuint)req.Length, data, (nuint)(s?.Data.Length ?? 0), out var p, out var r);
            Native.Consume(status, r).Json?.Dispose();
            Handle = new(p);
        }
    }
    public void Dispose() => Handle.Dispose();
}

internal sealed class PreparedChannelHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal PreparedChannelHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_channel_free(handle); return true; }
}

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_channel_prepare(byte[] req, nuint n, byte* data, nuint len, out IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_channel_free(IntPtr p);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_full_message(WriterHandle w, PreparedChannelHandle c, NativeHeader* h, byte* data, nuint len, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_private(WriterHandle w, byte opcode, [MarshalAs(UnmanagedType.I1)] bool chunks, byte* data, nuint len, out Result result);
}

public sealed partial class McapWriter
{
    public void WriteMessage(McapMessage message)
    {
        ArgumentNullException.ThrowIfNull(message);
        using var channel = new McapPreparedChannel(message.Channel);
        WriteMessage(channel, new(message.Channel.Id, message.Sequence, message.LogTime, message.PublishTime), message.Data);
    }

    public unsafe void WriteMessage(McapPreparedChannel channel, in McapMessageHeader header, ReadOnlySpan<byte> data)
    {
        ArgumentNullException.ThrowIfNull(channel);
        if (header.ChannelId != channel.Id) throw new ArgumentException("Header and prepared channel IDs differ.", nameof(header));
        lock (gate)
        {
            Check();
            ObjectDisposedException.ThrowIf(channel.Handle.IsClosed, channel);
            var h = new Native.NativeHeader { ChannelId = header.ChannelId, Sequence = header.Sequence, LogTime = header.LogTime, PublishTime = header.PublishTime };
            try
            {
                fixed (byte* p = data)
                {
                    var status = Native.fm_writer_full_message(handle, channel.Handle, &h, p, (nuint)data.Length, out var r);
                    if (status < 0) { var error = Native.ConsumeError(r); handle.Bridge?.ThrowIfError(); throw error; }
                }
            }
            catch { failed = true; throw; }
        }
    }

    public McapSummary Finish() { lock (gate) { Complete(); return GetSummary(); } }
}
