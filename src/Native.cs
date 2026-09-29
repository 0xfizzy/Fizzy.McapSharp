using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

internal static class Native
{
    private const string Library = "fizzy_mcap_native";
    [StructLayout(LayoutKind.Sequential)]
    internal struct Result { public IntPtr Json; public nuint JsonLength; public IntPtr Data; public nuint DataLength; public ulong Value; }
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern uint fm_abi_version();
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern int fm_writer_open(byte[] request, nuint length, out IntPtr handle, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern int fm_writer_call(WriterHandle handle, uint operation, byte[] request, nuint length, byte[] data, nuint dataLength, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern void fm_writer_free(IntPtr handle);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern int fm_reader_open(byte[] request, nuint length, out IntPtr handle, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern int fm_reader_next(ReaderHandle handle, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern void fm_reader_free(IntPtr handle);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern void fm_buffer_free(IntPtr data, nuint length);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern int fm_validate(byte[] request, nuint length, out Result result);
    internal static bool IsSupportedPlatform(bool windows, bool linux, Architecture architecture) =>
        (windows && architecture == Architecture.X64) ||
        (linux && architecture is Architecture.X64 or Architecture.Arm64);

    internal static void EnsureAvailable()
    {
        if (!IsSupportedPlatform(OperatingSystem.IsWindows(), OperatingSystem.IsLinux(), RuntimeInformation.ProcessArchitecture))
            throw new PlatformNotSupportedException("Fizzy.McapSharp supports Windows x64 and glibc Linux x64/ARM64 only.");
        if (fm_abi_version() != 1) throw new McapException("Incompatible Fizzy.McapSharp native ABI.");
    }
    internal static byte[] Request(object value) => JsonSerializer.SerializeToUtf8Bytes(value);
    internal static (JsonDocument? Json, byte[] Data, ulong Value) Consume(int status, Result result)
    {
        try
        {
            var jsonBytes = Copy(result.Json, result.JsonLength);
            if (status < 0) throw new McapException(Encoding.UTF8.GetString(jsonBytes));
            return (jsonBytes.Length == 0 ? null : JsonDocument.Parse(jsonBytes), Copy(result.Data, result.DataLength), result.Value);
        }
        finally { fm_buffer_free(result.Json, result.JsonLength); fm_buffer_free(result.Data, result.DataLength); }
    }
    private static byte[] Copy(IntPtr pointer, nuint size)
    {
        var length = checked((int)size);
        if (length == 0) return [];
        var result = new byte[length]; Marshal.Copy(pointer, result, 0, length); return result;
    }
}
internal sealed class WriterHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal WriterHandle(IntPtr value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.fm_writer_free(handle); return true; }
}
internal sealed class ReaderHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal ReaderHandle(IntPtr value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.fm_reader_free(handle); return true; }
}
