using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>An immutable native snapshot. Preparing does not register or write declarations.</summary>
public sealed class McapPreparedChannel : IDisposable
{
    internal readonly PreparedChannelHandle Handle;
    /// <summary>Channel ID captured during preparation; message headers must use this ID.</summary>
    public ushort Id { get; }
    /// <summary>Copies channel metadata and schema bytes into an immutable descriptor without registering declarations.</summary>
    public unsafe McapPreparedChannel(McapChannel channel)
    {
        ArgumentNullException.ThrowIfNull(channel);
        ArgumentNullException.ThrowIfNull(channel.Topic);
        ArgumentNullException.ThrowIfNull(channel.MessageEncoding);
        ArgumentValidation.ValidateMetadata(channel.Metadata);
        if (channel.Schema is { } schema)
        {
            ArgumentNullException.ThrowIfNull(schema.Name);
            ArgumentNullException.ThrowIfNull(schema.Encoding);
            ArgumentNullException.ThrowIfNull(schema.Data);
        }
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
    /// <summary>Releases the descriptor after the final synchronous write. Do not dispose concurrently with use.</summary>
    public void Dispose() => Handle.Dispose();
}
