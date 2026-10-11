namespace Fizzy.McapSharp;

internal sealed partial class AsyncReaderCore
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();

    void CheckDescriptions()
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        if (failed) throw new InvalidOperationException("Reader failed; open a new reader.");
        if (active) throw new InvalidOperationException("Consume the outstanding operation before querying declarations.");
    }

    /// <summary>Returns a cached immutable channel and its schema encountered by expanded record or message-lease reading.
    /// Top-level chunk delivery cannot collect declarations inside chunks. Consume each outstanding ValueTask before calling;
    /// failed or disposed readers reject lookup. An unknown ID throws McapException without terminating the reader.
    /// The returned description remains valid after advancement or disposal. First lookup may allocate.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate)
        {
            CheckDescriptions();
            if (!descriptions.TryGetValue(id, out var description))
                descriptions.Add(id, description = new(parser.DescribeChannel(id), schemaDescriptions));
            return description;
        }
    }

    /// <summary>Returns a cached immutable schema encountered by message-lease reading, including schemas
    /// not yet referenced by a channel. The same operation, failure and lifetime rules as GetChannelDescription apply.</summary>
    public McapSchemaDescription GetSchemaDescription(ushort id)
    {
        lock (gate)
        {
            CheckDescriptions();
            if (!schemaDescriptions.TryGetValue(id, out var description))
                schemaDescriptions.Add(id, description = new(parser.DescribeSchema(id)));
            return description;
        }
    }
}

