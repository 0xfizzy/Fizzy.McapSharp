using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

/// <summary>An immutable declaration snapshot that can be shared across messages and threads.</summary>
public sealed class McapSchemaDescription
{
    readonly byte[] data;
    public ushort Id { get; }
    public string Name { get; }
    public string Encoding { get; }
    public ReadOnlySpan<byte> Data => data;
    internal McapSchemaDescription(McapSchema schema) => (Id, Name, Encoding, data) = (schema.Id, schema.Name, schema.Encoding, schema.Data);
}
public sealed class McapChannelDescription
{
    public ushort Id { get; }
    public string Topic { get; }
    public string MessageEncoding { get; }
    public McapSchemaDescription? Schema { get; }
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
public sealed partial class McapReadSession
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
public sealed partial class McapBufferReader
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
public sealed partial class McapIndexSnapshot
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
