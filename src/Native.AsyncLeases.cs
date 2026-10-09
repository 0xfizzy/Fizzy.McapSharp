using System.Runtime.InteropServices;
using System.Threading.Tasks.Sources;

namespace Fizzy.McapSharp;

internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_lease_step(EngineHandle engine, nuint count, nuint target, out IntPtr batch, out ReadEvent e, out Result result);
}
