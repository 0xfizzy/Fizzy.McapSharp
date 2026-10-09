using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;
internal static partial class Native
{
    const string Library = "fizzy_mcap_native";
    [StructLayout(LayoutKind.Sequential)]
    internal struct Result
    {
        public IntPtr Json;
        public nuint JsonLength;
        public IntPtr Data;
        public nuint DataLength;
        public ulong Value;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct NativeHeader
    {
        public ushort ChannelId, Reserved;
        public uint Sequence;
        public ulong LogTime, PublishTime;
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal unsafe delegate int ReadCallback(IntPtr ctx, byte* dest, nuint len, nuint* read);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal unsafe delegate int WriteCallback(IntPtr ctx, byte* src, nuint len);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal unsafe delegate int SeekCallback(IntPtr ctx, long offset, int origin, ulong* position);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate int FlushCallback(IntPtr ctx);
    [StructLayout(LayoutKind.Sequential)]
    internal struct Callbacks
    {
        public IntPtr Context;
        public IntPtr Read, Write, Seek, Flush;
        public uint Seekable;
    }

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint fm_abi_version();
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_open(byte[] req, nuint len, Callbacks* cb, out IntPtr handle, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_message(WriterHandle h, NativeHeader* header, byte* data, nuint len, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_writer_call(WriterHandle h, uint op, byte[] req, nuint len, byte* data, nuint dataLen, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_writer_release(IntPtr h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_open(byte[] req, nuint len, Callbacks* cb, out IntPtr h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_next(ReaderHandle h, byte* dest, nuint capacity, out NativeHeader header, out byte opcode, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_describe(ReaderHandle h, uint kind, ushort id, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_release(IntPtr h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_buffer_free(IntPtr p, nuint n);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_validate(byte[] req, nuint len, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_summary(ReaderHandle h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_record_at(ReaderHandle h, ulong offset, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_snapshot_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_lease_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_operation_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_channel_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_chunk_index_release(IntPtr p, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_release(IntPtr p, out Result result);
    internal static byte[] Request(object v) => JsonSerializer.SerializeToUtf8Bytes(v);
    internal static bool IsSupportedPlatform(bool windows, bool linux, Architecture architecture) => (windows && architecture == Architecture.X64) || (linux && architecture is Architecture.X64 or Architecture.Arm64);
    internal static void EnsureAvailable()
    {
        if (!IsSupportedPlatform(OperatingSystem.IsWindows(), OperatingSystem.IsLinux(), RuntimeInformation.ProcessArchitecture))
            throw new PlatformNotSupportedException("Fizzy.McapSharp supports Windows x64 and glibc Linux x64/ARM64 only.");
        if (fm_abi_version() != 15)
            throw new McapException("Incompatible native ABI.");
    }

    internal static McapException ConsumeError(Result r, bool canContinueWriting = false)
    {
        try
        {
            var b = Copy(r.Json, r.JsonLength);
            return McapException.Decode(Encoding.UTF8.GetString(b), canContinueWriting);
        }
        finally
        {
            fm_buffer_free(r.Json, r.JsonLength);
            fm_buffer_free(r.Data, r.DataLength);
        }
    }

    internal static byte[] Copy(IntPtr p, nuint n)
    {
        if (n == 0)
            return [];
        var a = new byte[checked((int)n)];
        Marshal.Copy(p, a, 0, a.Length);
        return a;
    }

    internal static (JsonDocument? Json, byte[] Data, ulong Value) ConsumeReader(int status, Result result, StreamBridge? bridge)
    {
        if (status < Protocol.Status.Success)
        {
            var error = ConsumeError(result);
            if (bridge is not null) bridge.ThrowOperationError(error);
            throw error;
        }
        var consumed = Consume(status, result);
        try { bridge?.ThrowIfError(); return consumed; }
        catch { consumed.Json?.Dispose(); throw; }
    }

    internal static (JsonDocument? Json, byte[] Data, ulong Value) Consume(int status, Result r)
    {
        try
        {
            var j = Copy(r.Json, r.JsonLength);
            if (status < Protocol.Status.Success)
                throw McapException.Decode(Encoding.UTF8.GetString(j));
            return (j.Length == 0 ? null : JsonDocument.Parse(j), Copy(r.Data, r.DataLength), r.Value);
        }
        finally
        {
            fm_buffer_free(r.Json, r.JsonLength);
            fm_buffer_free(r.Data, r.DataLength);
        }
    }
}

internal abstract class OwnedNativeHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    readonly StreamBridge? bridge;
    Exception? releaseError;
    bool explicitRelease, transfer;
    internal bool NativeReleased { get; private set; }
    protected virtual bool DependenciesReleased => NativeReleased;
    protected OwnedNativeHandle(IntPtr value, StreamBridge? bridge = null) : base(true)
    { SetHandle(value); this.bridge = bridge; }
    protected abstract int ReleaseNative(IntPtr value, out Native.Result result);
    internal Stream Transfer()
    {
        var stream = bridge?.Stream ?? throw new NotSupportedException("Only Stream-backed sessions can transfer ownership.");
        transfer = true;
        Dispose();
        return stream;
    }
    protected override void Dispose(bool disposing)
    {
        explicitRelease = disposing;
        base.Dispose(disposing);
        var error = releaseError;
        releaseError = null;
        if (disposing && error is not null) System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(error).Throw();
    }
    protected override bool ReleaseHandle()
    {
        Exception? error = null;
        try
        {
            int status = ReleaseNative(handle, out var result);
            NativeReleased = true; // The release ABI consumes the native owner even when Drop reports a panic.
            Native.Consume(status, result).Json?.Dispose();
        }
        catch (Exception e) { error = e; }
        try { if (DependenciesReleased) bridge?.Release(transfer && error is null); }
        catch (Exception e) { error = error is null ? e : new AggregateException(error, e); }
        if (explicitRelease) releaseError = error;
        return error is null;
    }
}

internal sealed class WriterHandle : OwnedNativeHandle
{
    internal readonly StreamBridge? Bridge;
    internal WriterHandle(IntPtr p, StreamBridge? bridge = null) : base(p, bridge) => Bridge = bridge;
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_writer_release(value, out result);
}

internal sealed class ReaderHandle : OwnedNativeHandle
{
    internal readonly StreamBridge? Bridge;
    internal ReaderHandle(IntPtr p, StreamBridge? bridge = null) : base(p, bridge) => Bridge = bridge;
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_reader_release(value, out result);
}
