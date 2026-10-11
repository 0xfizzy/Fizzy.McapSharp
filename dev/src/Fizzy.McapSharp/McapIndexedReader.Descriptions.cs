using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

public sealed partial class McapIndexedReader
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable declaration from the summary. Its managed schema bytes and metadata remain valid after indexed-reader disposal.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        CheckBeforeLock();
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
}
