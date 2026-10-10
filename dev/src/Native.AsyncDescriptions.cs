using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_describe(EngineHandle handle, uint kind, ushort id, out Result result);
}
