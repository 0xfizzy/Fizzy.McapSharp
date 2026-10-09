using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapBufferReader
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
