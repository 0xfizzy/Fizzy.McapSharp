using System.Collections.ObjectModel;

namespace Fizzy.McapSharp;

internal sealed partial class ReadCursorCore
{
    readonly Dictionary<ushort, McapChannelDescription> descriptions = new();
    readonly Dictionary<ushort, McapSchemaDescription> schemaDescriptions = new();
    /// <summary>Returns a cached immutable encountered or snapshot-provided channel and schema. The result survives cursor disposal; unknown IDs do not advance or terminate the cursor.</summary>
    public McapChannelDescription GetChannelDescription(ushort id)
    {
        lock (gate) { Check(); if (!descriptions.TryGetValue(id, out var description)) descriptions.Add(id, description = new(GetChannel(id), schemaDescriptions)); return description; }
    }
    public McapSchema GetSchema(ushort id)
    {
        lock (gate)
        {
            Check();
            int status = Native.fm_buffer_reader_schema(handle, id, out var result);
            var response = Native.Consume(status, result);
            using var json = response.Json!;
            var value = json.RootElement;
            return new(id, value.GetProperty("name").GetString()!, value.GetProperty("encoding").GetString()!, response.Data);
        }
    }
    public McapSchemaDescription GetSchemaDescription(ushort id)
    {
        lock (gate)
        {
            Check();
            if (!schemaDescriptions.TryGetValue(id, out var description))
                schemaDescriptions.Add(id, description = new(GetSchema(id)));
            return description;
        }
    }
}

internal static partial class Native
{
    [System.Runtime.InteropServices.DllImport(Library, CallingConvention = System.Runtime.InteropServices.CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_schema(BufferReaderHandle reader, ushort id, out Result result);
}
