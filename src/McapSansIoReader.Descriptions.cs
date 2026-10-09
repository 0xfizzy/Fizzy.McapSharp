namespace Fizzy.McapSharp;

public sealed partial class McapSansIoReader
{
    // Called only by the async owner's serialized cold declaration queries.
    internal McapChannel DescribeChannel(ushort id)
    {
        int status = Native.fm_engine_describe(handle, Protocol.DeclarationKind.Channel, id, out var result);
        return McapBufferReader.DecodeChannel(Native.Consume(status, result));
    }

    internal McapSchema DescribeSchema(ushort id)
    {
        int status = Native.fm_engine_describe(handle, Protocol.DeclarationKind.Schema, id, out var result);
        var response = Native.Consume(status, result);
        using var json = response.Json!;
        var schema = json.RootElement;
        return new(id, schema.GetProperty("name").GetString()!, schema.GetProperty("encoding").GetString()!, response.Data);
    }
}

