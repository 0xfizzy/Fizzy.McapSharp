using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

/// <summary>An immutable declaration snapshot that can be shared across messages and threads.</summary>
public sealed class McapSchemaDescription
{
    readonly byte[] data;
    /// <summary>Declaration ID from the recording; meaningful within its declaration context.</summary>
    public ushort Id { get; }
    /// <summary>Schema name as declared in the recording.</summary>
    public string Name { get; }
    /// <summary>Schema encoding identifier; interpretation is application-defined.</summary>
    public string Encoding { get; }
    /// <summary>Read-only schema bytes retained independently of the reader lifetime.</summary>
    public ReadOnlySpan<byte> Data => data;
    internal McapSchemaDescription(McapSchema schema) => (Id, Name, Encoding, data) = (schema.Id, schema.Name, schema.Encoding, schema.Data);
}
/// <summary>Immutable resolved channel declaration shareable across messages and threads; survives reader disposal.</summary>
public sealed class McapChannelDescription
{
    /// <summary>Declaration ID from the recording; meaningful within its declaration context.</summary>
    public ushort Id { get; }
    /// <summary>Exact channel topic string.</summary>
    public string Topic { get; }
    /// <summary>Message payload encoding identifier; decoding is application-defined.</summary>
    public string MessageEncoding { get; }
    /// <summary>Resolved immutable schema, or null for schema ID zero.</summary>
    public McapSchemaDescription? Schema { get; }
    /// <summary>Immutable metadata snapshot retained independently of the reader.</summary>
    public IReadOnlyDictionary<string, string> Metadata { get; }
    internal McapChannelDescription(McapChannel channel, Dictionary<ushort, McapSchemaDescription> schemas)
    {
        Id = channel.Id; Topic = channel.Topic; MessageEncoding = channel.MessageEncoding;
        if (channel.Schema is not null)
        {
            if (!schemas.TryGetValue(channel.Schema.Id, out var schema)) schemas.Add(channel.Schema.Id, schema = new(channel.Schema));
            Schema = schema;
        }
        Metadata = new ReadOnlyDictionary<string, string>(new Dictionary<string, string>(channel.Metadata));
    }
}
