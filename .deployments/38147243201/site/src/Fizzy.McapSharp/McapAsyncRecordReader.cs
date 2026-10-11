namespace Fizzy.McapSharp;

/// <summary>Incremental asynchronous record reader. Owns its Stream unless leaveOpen is true. Allows one outstanding operation, which must be consumed exactly once. Expanded reads validate record bodies and declaration references independently of CRC options.</summary>
public sealed class McapAsyncRecordReader : IDisposable, IAsyncDisposable
{
    readonly AsyncReaderCore core;
    /// <summary>Opens a readable Stream without prebuffering it. inputBufferSize limits each I/O transfer, not parser storage.</summary>
    public McapAsyncRecordReader(Stream stream, McapReaderOptions? options = null, bool leaveOpen = false, int inputBufferSize = 65536)
        => core = new(stream, options, leaveOpen, inputBufferSize, messages: false);
    /// <summary>Copies the next record body. By default, chunks are expanded; with EmitChunks enabled, returns top-level records including encoded Chunk bodies without validating records inside those chunks. Keep destination valid until consuming the ValueTask exactly once. BufferTooSmall retains the record; I/O, parsing and cancellation errors terminate reading.</summary>
    public ValueTask<McapRecordReadResult> ReadNextRecordAsync(Memory<byte> destination, CancellationToken cancellationToken = default)
        => core.ReadNextRecordAsync(destination, cancellationToken);
    /// <summary>Returns an immutable encountered channel; consume the outstanding operation first. Missing IDs do not terminate reading; failures and disposal reject lookup.</summary>
    public McapChannelDescription GetChannelDescription(ushort id) => core.GetChannelDescription(id);
    /// <summary>Returns an immutable encountered schema, including unreferenced schemas. The result survives reader disposal.</summary>
    public McapSchemaDescription GetSchemaDescription(ushort id) => core.GetSchemaDescription(id);
    /// <summary>Releases the parser and transfers its Stream after the outstanding operation has been consumed. Release failure prevents transfer.</summary>
    public Stream IntoInner() => core.IntoInner();
    /// <summary>Releases parser state before owned input. Outstanding operations must be consumed first. Cleanup errors are reported once.</summary>
    public void Dispose() => core.Dispose();
    /// <summary>Performs synchronous resource release and returns a completed ValueTask; does not call Stream.DisposeAsync.</summary>
    public ValueTask DisposeAsync() => core.DisposeAsync();
}
