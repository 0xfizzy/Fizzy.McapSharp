using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapIndexSnapshot
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable declaration from the summary. Its managed schema bytes and metadata remain valid after snapshot disposal.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
