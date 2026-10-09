using System.Text.Json;

namespace Fizzy.McapSharp;

internal static class JsonSupport
{
    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true
    };
}
