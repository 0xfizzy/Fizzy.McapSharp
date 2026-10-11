namespace Fizzy.McapSharp;

/// <summary>Serialized random access over one seekable file or Stream, without copying the complete input. Offsets are relative to the initial MCAP position. Parsing or I/O failure terminates this reader; independent copied indexed readers survive disposal.</summary>
public sealed class McapRecordAccessor : IDisposable
{
    readonly ReadSessionCore core;
    internal McapRecordAccessor(ReadSessionCore core) => this.core = core;
    /// <summary>Reads an independent complete summary, or null if absent. This does not validate the complete file.</summary>
    public McapSummary? GetSummary() => core.GetSummary();
    /// <summary>Copies a record body at the given byte offset, preserving source position.</summary>
    public McapRawRecord ReadRecordAt(ulong offset) => core.ReadRecordAt(offset);
    /// <summary>Copies a record body into caller storage. BufferTooSmall reports required capacity without advancing; errors terminate this reader.</summary>
    public McapReadStatus ReadRecordAt(ulong offset, Span<byte> destination, out byte opcode, out ulong length)
        => core.ReadRecordAt(offset, destination, out opcode, out length);
    /// <summary>Copies the encoded Chunk record and validates its opcode and indexed length.</summary>
    public McapRawRecord ReadChunk(McapChunkIndex index) => core.ReadChunk(index);
    /// <summary>Copies indexed message-index groups in channel-ID order, including empty groups.</summary>
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapChunkIndex index) => core.ReadMessageIndexes(index);
    /// <summary>Copies the complete input into an independent indexed reader while preserving source position. Dispose the returned reader; cache allowance excludes this input copy.</summary>
    public McapIndexedReader CreateCopiedIndexedReader(McapIndexedReaderOptions? options = null) => core.CreateCopiedIndexedReader(options);
    /// <summary>Releases native state and transfers the underlying Stream. Path-backed readers do not have a managed Stream to transfer.</summary>
    public Stream IntoInner() => core.IntoInner();
    /// <summary>Releases native state before owned input and reports cleanup failures. Repeated disposal is a no-op.</summary>
    public void Dispose() => core.Dispose();
}
