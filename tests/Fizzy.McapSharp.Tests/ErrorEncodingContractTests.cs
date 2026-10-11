using System.Text;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ErrorEncodingContractTests
{
    [Fact]
    public void DetailsDistinguishMessageOnlyEmptyAndStructuredErrors()
    {
        Assert.Equal(System.Text.Json.JsonValueKind.Undefined, new McapException("message").Details.ValueKind);
        using var output = new MemoryStream();
        using var writer = new McapWriter(output, leaveOpen: true);
        var invalidSchema = Assert.Throws<McapException>(() => writer.RegisterSchema(0, "s", "raw", []));
        Assert.Equal(McapErrorKind.InvalidSchemaId, invalidSchema.Kind);
        Assert.Equal(System.Text.Json.JsonValueKind.Object, invalidSchema.Details.ValueKind);
        Assert.Empty(invalidSchema.Details.EnumerateObject());
        var missing = Assert.Throws<McapException>(() => writer.WriteToKnownChannel(new(31, 43, 53, 61), []));
        Assert.Equal(McapErrorKind.UnknownChannel, missing.Kind);
        Assert.Equal(31u, missing.Details.GetProperty("channel_id").GetUInt32());
        Assert.Equal(43u, missing.Details.GetProperty("sequence").GetUInt32());
        Assert.True(missing.CanContinueWriting);
        var future = McapException.Decode("{\"kind\":\"Binding\",\"message\":\"diagnostic\",\"details\":{\"future\":true}}");
        Assert.True(future.Details.GetProperty("future").GetBoolean());
        Assert.Equal(System.Text.Json.JsonValueKind.Null,
            McapException.Decode("{\"kind\":\"Binding\",\"message\":\"diagnostic\",\"details\":null}").Details.ValueKind);
    }

    [Fact]
    public void ConflictingChannelNamesAreBoundedWithoutBreakingUtf8()
    {
        using var output = new MemoryStream();
        using var writer = new McapWriter(output, leaveOpen: true);
        writer.RegisterChannel(1, "original", "raw");
        string topic = new('\u754c', 10000);
        var error = Assert.Throws<McapException>(() => writer.RegisterChannel(1, topic, "raw"));
        Assert.Equal(McapErrorKind.ConflictingChannels, error.Kind);
        Assert.True(Encoding.UTF8.GetByteCount(error.Message) <= 256);
        Assert.EndsWith("[truncated]", error.Message);
        string name = error.Details.GetProperty("name").GetString()!;
        Assert.True(Encoding.UTF8.GetByteCount(name) <= 128);
        Assert.EndsWith("[truncated]", name);
        Assert.DoesNotContain("\ufffd", name);
        Assert.True(error.Details.GetProperty("truncated").GetBoolean());
        Assert.True(error.CanContinueWriting);
    }
}
