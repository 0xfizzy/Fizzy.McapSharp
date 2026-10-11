namespace Fizzy.McapSharp;

/// <summary>Records from one encoded Chunk body, without its outer record header. Validation is local to the chunk; declarations may live elsewhere.</summary>
public sealed class McapChunkReader : IDisposable
{
    readonly IRecordSource core;
    internal McapChunkReader(IRecordSource core) => this.core = core;
    /// <summary>Copies the supplied input into independent storage; subsequent advancement parses records.</summary>
    public McapChunkReader(ReadOnlySpan<byte> data)
        => core = new ReadCursorCore(data, CursorMode.ChunkRecords);
    /// <summary>Copies a raw record body into caller storage. BufferTooSmall retains the item and reports required capacity; parse failures terminate reading.</summary>
    public McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
        => core.ReadNextRecord(destination, out opcode, out length);
    /// <summary>Advances this reader and returns independent mutable copies of record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords() => core.ReadRecords();
    /// <summary>Releases parser state before input. Independent returned records remain valid; repeated disposal is a no-op.</summary>
    public void Dispose() => core.Dispose();
}
