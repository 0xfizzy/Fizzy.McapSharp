namespace Fizzy.McapSharp;

/// <summary>Expanded file records with record-body and declaration-reference validation independent of CRC options.</summary>
public sealed class McapChunkFlattener : IDisposable
{
    /// <summary>Creation-time validation scope; remains available after failure or disposal. Only StrictFullScan supports ValidateRemaining and, on message readers, prefix recovery.</summary>
    public McapValidationCapability ValidationCapability => ((IReadProgress)core).ValidationCapability;
    readonly IRecordSource core;
    internal McapChunkFlattener(IRecordSource core) => this.core = core;
    /// <summary>Copies the supplied input into independent storage; subsequent advancement parses records.</summary>
    public McapChunkFlattener(ReadOnlySpan<byte> data, McapReaderOptions? options = null)
        => core = new ReadCursorCore(data, CursorMode.ExpandedRecords, options);
    /// <summary>Maps input without copying. Keep the file unchanged until this reader and all storage leases have been disposed.</summary>
    public static McapChunkFlattener OpenMapped(string path, McapReaderOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        return new(new ReadCursorCore([], CursorMode.ExpandedRecords, options, Path.GetFullPath(path)));
    }
    /// <summary>Copies a raw record body into caller storage. BufferTooSmall retains the item and reports required capacity; parse failures terminate reading.</summary>
    public McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
        => core.ReadNextRecord(destination, out opcode, out length);
    /// <summary>Advances this reader and returns independent mutable copies of record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords() => core.ReadRecords();
    /// <summary>Releases parser state before input. Independent returned records remain valid; repeated disposal is a no-op.</summary>
    public void Dispose() => core.Dispose();
    /// <summary>True after this scan reaches EOF without failure; this alone does not prove full-file validation.</summary>
    public bool IsScanComplete => ((IReadProgress)core).IsScanComplete;
    /// <summary>True only after a strict complete expanded scan. Indexed, chunk-local and raw-message reads cannot prove full-file validation.</summary>
    public bool IsFullyValidated => ((IReadProgress)core).IsFullyValidated;
    /// <summary>Native record count from a completed scan, not a live progress counter.</summary>
    public ulong ScannedRecordCount => ((IReadProgress)core).ScannedRecordCount;
    /// <summary>Completes a scan that has performed strict checks from creation and returns its record count. Requires StrictFullScan; rejection does not advance or fail a usable reader. Previously skipped checks cannot be retroactively validated.</summary>
    public ulong ValidateRemaining() => ((IReadProgress)core).ValidateRemaining();
    /// <summary>Releases native state and transfers the underlying Stream. Copied, mapped and path input have no managed Stream to transfer.</summary>
    public Stream IntoInner() => ((IReadProgress)core).IntoInner();
    /// <summary>Copies an encountered or retained summary schema without advancing. Unknown IDs do not terminate reading.</summary>
    public McapSchema GetSchema(ushort id) => ((IDeclarationSource)core).GetSchema(id);
    /// <summary>Copies an encountered or retained summary channel and schema without advancing. Unknown IDs do not terminate reading.</summary>
    public McapChannel GetChannel(ushort id) => ((IDeclarationSource)core).GetChannel(id);
    /// <summary>Returns a cached immutable encountered or retained summary schema. The description survives reader disposal.</summary>
    public McapSchemaDescription GetSchemaDescription(ushort id) => ((IDeclarationSource)core).GetSchemaDescription(id);
    /// <summary>Returns a cached immutable encountered or retained summary channel. Failed or disposed readers reject lookup.</summary>
    public McapChannelDescription GetChannelDescription(ushort id) => ((IDeclarationSource)core).GetChannelDescription(id);
}
