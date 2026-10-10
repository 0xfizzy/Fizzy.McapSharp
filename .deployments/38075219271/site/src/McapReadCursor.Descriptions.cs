using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapReadCursor
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable encountered or snapshot-provided channel and schema. The result survives cursor disposal; unknown IDs do not advance or terminate the cursor.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
