using System.Text.Json;
using System.Text.Json.Serialization;

namespace Fizzy.McapSharp;

internal static class JsonSupport
{
    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true,
        Converters = { new BinaryArrayConverter(), new SummaryConverter() }
    };

    sealed class SummaryConverter : JsonConverter<McapSummary>
    {
        public override McapSummary Read(ref Utf8JsonReader reader, Type type, JsonSerializerOptions options)
        {
            using var document = JsonDocument.ParseValue(ref reader);
            var root = document.RootElement;
            var schemas = root.GetProperty("schemas").Deserialize<Dictionary<ushort, McapSchema>>(options)!;
            var dependencies = root.TryGetProperty("schemaDependencies", out var extra)
                ? extra.Deserialize<Dictionary<ushort, McapSchema>>(options)! : new();
            var channels = new Dictionary<ushort, McapChannel>();
            foreach (var property in root.GetProperty("channels").EnumerateObject())
            {
                var channel = property.Value;
                ushort schemaId = channel.GetProperty("schemaId").GetUInt16();
                McapSchema? schema = null;
                if (schemaId != 0 && !schemas.TryGetValue(schemaId, out schema) && !dependencies.TryGetValue(schemaId, out schema))
                    throw new JsonException("Summary channel schema is missing.");
                ushort id = channel.GetProperty("id").GetUInt16();
                channels.Add(id, new(id, channel.GetProperty("topic").GetString()!, channel.GetProperty("messageEncoding").GetString()!,
                    schema, channel.GetProperty("metadata").Deserialize<Dictionary<string, string>>(options)!));
            }
            return new(root.GetProperty("statistics").Deserialize<McapStatistics>(options),
                root.GetProperty("chunkIndexes").Deserialize<McapChunkIndex[]>(options)!,
                root.GetProperty("attachmentIndexes").Deserialize<McapAttachmentIndex[]>(options)!,
                root.GetProperty("metadataIndexes").Deserialize<McapMetadataIndex[]>(options)!, schemas, channels);
        }
        public override void Write(Utf8JsonWriter writer, McapSummary value, JsonSerializerOptions options)
            => throw new NotSupportedException("The private native summary decoder is read-only.");
    }

    sealed class BinaryArrayConverter : JsonConverter<byte[]>
    {
        public override byte[] Read(ref Utf8JsonReader reader, Type type, JsonSerializerOptions options)
        {
            if (reader.TokenType == JsonTokenType.String) return reader.GetBytesFromBase64();
            if (reader.TokenType != JsonTokenType.StartArray) throw new JsonException("Expected binary bytes.");
            var measure = reader;
            int length = 0;
            while (measure.Read() && measure.TokenType != JsonTokenType.EndArray) { measure.GetByte(); length = checked(length + 1); }
            if (measure.TokenType != JsonTokenType.EndArray) throw new JsonException("Incomplete binary array.");
            var data = new byte[length];
            for (int i = 0; i < length; i++) { reader.Read(); data[i] = reader.GetByte(); }
            reader.Read();
            return data;
        }
        public override void Write(Utf8JsonWriter writer, byte[] value, JsonSerializerOptions options) => writer.WriteBase64StringValue(value);
    }
}
