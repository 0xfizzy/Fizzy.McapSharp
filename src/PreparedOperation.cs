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
    public static McapPreparedOperation Schema(string name, string encoding, ReadOnlySpan<byte> data, ushort? id = null) => new(1, new { name, encoding, id }, data);
    public static McapPreparedOperation Channel(string topic, string encoding, ushort schemaId = 0, IReadOnlyDictionary<string, string>? metadata = null, ushort? id = null) => new(2, new { topic, encoding, schema_id = schemaId, metadata = metadata ?? new Dictionary<string, string>(), id });
    public static McapPreparedOperation Metadata(string name, IReadOnlyDictionary<string, string> metadata) => new(4, new { name, metadata });
    public static McapPreparedOperation Attachment(string name, string mediaType, ulong logTime, ulong createTime) => new(5, new { name, media_type = mediaType, log_time = logTime, create_time = createTime });
    public static McapPreparedOperation StartAttachment(string name, string mediaType, ulong logTime, ulong createTime, ulong length) => new(8, new { name, media_type = mediaType, log_time = logTime, create_time = createTime, length });
    public void Dispose() => Handle.Dispose();
}

public sealed partial class McapWriter
{
    /// <summary>Runs the prepared operation synchronously; returns the registered ID for schema/channel operations, otherwise zero. Payload is consumed only by Attachment. Schema uses the bytes copied when prepared; other operations ignore payload. StartAttachment declares the length but consumes no body; follow with WriteAttachmentBytes and FinishAttachment. Writer failure and audited rejection rules still apply.</summary>
    public unsafe ulong WritePrepared(McapPreparedOperation operation, ReadOnlySpan<byte> payload = default)
    {
        ArgumentNullException.ThrowIfNull(operation);
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
                    if (operation.Operation == 8) attachment = true;
                    return r.Value;
                }
            }
            catch { if (!safeRejection) failed = true; throw; }
        }
    }
}
internal sealed class OperationHandle : OwnedNativeHandle
{
    internal OperationHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_operation_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_operation_prepare(uint op, byte[] req, nuint n, byte* data, nuint len, out IntPtr p, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_prepared(WriterHandle h, OperationHandle op, byte* data, nuint len, out Result r);
}
