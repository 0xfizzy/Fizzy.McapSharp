using System.Text.Json;

namespace Fizzy.McapSharp;

// Decodes declaration responses shared by all reader entry points.
internal static class DeclarationDecoder
{
    internal static McapChannel Channel((JsonDocument? Json, byte[] Data, ulong Value) response)
    {
        using var j = response.Json!;
        var c = j.RootElement; var s = c.GetProperty("schema");
        McapSchema? schema = s.ValueKind == JsonValueKind.Null ? null : new(s.GetProperty("id").GetUInt16(), s.GetProperty("name").GetString()!, s.GetProperty("encoding").GetString()!, response.Data);
        return new(c.GetProperty("id").GetUInt16(), c.GetProperty("topic").GetString()!, c.GetProperty("messageEncoding").GetString()!, schema, c.GetProperty("metadata").Deserialize<Dictionary<string, string>>()!);
    }
}
