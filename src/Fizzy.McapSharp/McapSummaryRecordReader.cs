namespace Fizzy.McapSharp;

/// <summary>Retained summary records. The reader survives disposal of its originating writer, snapshot or summary parser; it does not scan file contents.</summary>
public sealed class McapSummaryRecordReader : IDisposable
{
    readonly IRecordSource core;
    internal McapSummaryRecordReader(IRecordSource core) => this.core = core;
    /// <summary>Returns an independent channel and schema from the retained summary, without advancing the record iterator.</summary>
    public McapChannel GetChannel(ushort id) => ((ReadCursorCore)core).GetChannel(id);
    /// <summary>Returns a cached immutable channel from the retained summary; the description survives reader disposal.</summary>
    public McapChannelDescription GetChannelDescription(ushort id) => ((ReadCursorCore)core).GetChannelDescription(id);
    /// <summary>Copies a raw record body into caller storage. BufferTooSmall retains the item and reports required capacity; parse failures terminate reading.</summary>
    public McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
        => core.ReadNextRecord(destination, out opcode, out length);
    /// <summary>Advances this reader and returns independent mutable copies of record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords() => core.ReadRecords();
    /// <summary>Releases parser state before input. Independent returned records remain valid; repeated disposal is a no-op.</summary>
    public void Dispose() => core.Dispose();
    /// <summary>Copies an encountered or retained summary schema without advancing. Unknown IDs do not terminate reading.</summary>
    public McapSchema GetSchema(ushort id) => ((IDeclarationSource)core).GetSchema(id);
    /// <summary>Returns a cached immutable encountered or retained summary schema. The description survives reader disposal.</summary>
    public McapSchemaDescription GetSchemaDescription(ushort id) => ((IDeclarationSource)core).GetSchemaDescription(id);
}
