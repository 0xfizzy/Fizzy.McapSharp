using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapReadSession
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable channel and schema encountered by the sequential scan or loaded from a summary. The result survives session disposal. Lookup does not advance the scan or establish validation; unknown IDs do not terminate the session. First lookup may allocate.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
