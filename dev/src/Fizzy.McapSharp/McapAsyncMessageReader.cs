namespace Fizzy.McapSharp;

/// <summary>Incremental asynchronous resolved message reader. Owns its Stream unless leaveOpen is true. Allows one outstanding operation, which must be consumed exactly once. Expanded reads validate record bodies and declaration references independently of CRC options.</summary>
public sealed class McapAsyncMessageReader : IDisposable, IAsyncDisposable
{
    readonly AsyncReaderCore core;
    /// <summary>Opens a readable Stream without prebuffering it. inputBufferSize limits each I/O transfer, not parser storage.</summary>
    public McapAsyncMessageReader(Stream stream, McapReaderOptions? options = null, bool leaveOpen = false, int inputBufferSize = 65536)
        => core = new(stream, options, leaveOpen, inputBufferSize, messages: true);
    /// <summary>Returns retained message storage, or null at EOF. Consume each ValueTask exactly once. Failure publishes no partial lease and terminates reading. Leases survive advancement and reader disposal.</summary>
    public ValueTask<McapMessageBatchLease?> ReadBatchLeaseAsync(int maxMessages = 256, int targetPayloadBytes = 4 * 1024 * 1024, CancellationToken cancellationToken = default)
        => core.ReadBatchLeaseAsync(maxMessages, targetPayloadBytes, cancellationToken);
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
