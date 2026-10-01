using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;
internal static partial class Native
{
    const string Library = "fizzy_mcap_native";
    [StructLayout(LayoutKind.Sequential)]
    internal unsafe struct Result
    {
        public IntPtr Json;
        public nuint JsonLength;
        public IntPtr Data;
        public nuint DataLength;
        public ulong Value;
        public nuint ErrorLength;
        public fixed byte ErrorBytes[4096];
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
    internal static extern void fm_writer_free(IntPtr h);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_open(byte[] req, nuint len, Callbacks* cb, out IntPtr h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_next(ReaderHandle h, byte* dest, nuint capacity, out NativeHeader header, out byte opcode, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_describe(ReaderHandle h, uint kind, ushort id, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_reader_free(IntPtr h);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_buffer_free(IntPtr p, nuint n);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_validate(byte[] req, nuint len, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_summary(ReaderHandle h, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_reader_record_at(ReaderHandle h, ulong offset, out Result result);
    internal static byte[] Request(object v) => JsonSerializer.SerializeToUtf8Bytes(v);
    internal static bool IsSupportedPlatform(bool windows, bool linux, Architecture architecture) => (windows && architecture == Architecture.X64) || (linux && architecture is Architecture.X64 or Architecture.Arm64);
    internal static void EnsureAvailable()
    {
        if (!IsSupportedPlatform(OperatingSystem.IsWindows(), OperatingSystem.IsLinux(), RuntimeInformation.ProcessArchitecture))
            throw new PlatformNotSupportedException("Fizzy.McapSharp supports Windows x64 and glibc Linux x64/ARM64 only.");
        if (fm_abi_version() != 11)
            throw new McapException("Incompatible native ABI.");
    }

    internal static unsafe McapException ConsumeError(Result r, bool canContinueWriting = false)
    {
        try
        {
            return DecodeError(in r, canContinueWriting);
        }
        finally
        {
            fm_buffer_free(r.Json, r.JsonLength);
            fm_buffer_free(r.Data, r.DataLength);
        }
    }

    static unsafe McapException DecodeError(in Result r, bool canContinueWriting = false)
    {
        if (r.ErrorLength > 4096) return new McapException("Invalid native error length.");
        fixed (byte* p = r.ErrorBytes)
            return McapException.Decode(Encoding.UTF8.GetString(new ReadOnlySpan<byte>(p, (int)r.ErrorLength)), canContinueWriting);
    }

    internal static byte[] Copy(IntPtr p, nuint n)
    {
        if (n == 0)
            return [];
        var a = new byte[checked((int)n)];
        Marshal.Copy(p, a, 0, a.Length);
        return a;
    }

    internal static (JsonDocument? Json, byte[] Data, ulong Value) Consume(int status, Result r)
    {
        try
        {
            if (status < 0)
                throw DecodeError(in r);
            var j = Copy(r.Json, r.JsonLength);
            return (j.Length == 0 ? null : JsonDocument.Parse(j), Copy(r.Data, r.DataLength), r.Value);
        }
        finally
        {
            fm_buffer_free(r.Json, r.JsonLength);
            fm_buffer_free(r.Data, r.DataLength);
        }
    }
}

internal sealed class WriterHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal readonly StreamBridge? Bridge;
    internal WriterHandle(IntPtr p, StreamBridge? bridge = null) : base(true)
    {
        SetHandle(p);
        Bridge = bridge;
    }

    protected override bool ReleaseHandle()
    {
        Native.fm_writer_free(handle);
        Bridge?.Release();
        NativeStorageSignal.Pulse();
        return true;
    }
}

internal sealed class ReaderHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal readonly StreamBridge? Bridge;
    internal ReaderHandle(IntPtr p, StreamBridge? bridge = null) : base(true)
    {
        SetHandle(p);
        Bridge = bridge;
    }

    protected override bool ReleaseHandle()
    {
        Native.fm_reader_free(handle);
        Bridge?.Release();
        NativeStorageSignal.Pulse();
        return true;
    }
}
