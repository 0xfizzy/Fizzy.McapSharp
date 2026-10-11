using System.Runtime.InteropServices;
namespace Fizzy.McapSharp;
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_reader_message_record(ReaderHandle reader, byte* destination, nuint capacity, out NativeHeader header, out Result result);
}
