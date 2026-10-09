namespace Fizzy.McapSharp;

internal static class ArgumentValidation
{
    internal static void ValidateMetadata(IReadOnlyDictionary<string, string> metadata)
    {
        ArgumentNullException.ThrowIfNull(metadata);
        foreach (var entry in metadata)
            if (entry.Key is null || entry.Value is null)
                throw new ArgumentException("Metadata keys and values must not be null.", nameof(metadata));
    }
}
