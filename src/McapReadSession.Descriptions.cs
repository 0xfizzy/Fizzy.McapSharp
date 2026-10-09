using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapReadSession
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable encountered channel and schema. The result survives session disposal. Unknown IDs do not advance or terminate the session; first lookup may allocate.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
