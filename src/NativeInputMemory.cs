using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

// Only McapAsyncReader owns this memory. It cannot advance or release the parser until
// Stream.ReadAsync has completed and its ValueTask has been consumed.
internal sealed unsafe class NativeInputMemory(McapSansIoReader parser) : MemoryManager<byte>
{
    byte* pointer;
    int length;
    internal Memory<byte> Prepare(int size)
    {
        pointer = parser.PrepareInput(size); length = size;
        return Memory;
    }
    internal void Complete(int count)
    {
        if ((uint)count > (uint)length) throw new IOException("Stream returned an invalid read count.");
        parser.CompleteInput(count); pointer = null; length = 0;
    }
    public override Span<byte> GetSpan() => new(pointer, length);
    public override MemoryHandle Pin(int elementIndex = 0)
    {
        if ((uint)elementIndex > (uint)length) throw new ArgumentOutOfRangeException(nameof(elementIndex));
        return new(pointer + elementIndex);
    }
    public override void Unpin() { }
    protected override void Dispose(bool disposing) { pointer = null; length = 0; }
}
public sealed partial class McapSansIoReader
{
    internal unsafe byte* PrepareInput(int size)
    {
        int status = Native.fm_engine_input_buffer(handle, (nuint)size, out var pointer, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        if (status == 4) throw new McapMemoryBudgetUnavailableException();
        return pointer;
    }
    internal void CompleteInput(int count)
    {
        int status = Native.fm_engine_input_complete(handle, (nuint)count, out var result);
        if (status < 0) throw Native.ConsumeError(result);
    }
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_engine_input_buffer(EngineHandle engine, nuint size, out byte* pointer, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_input_complete(EngineHandle engine, nuint count, out Result result);
}
