using System.Text;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ErrorEncodingContractTests
{
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
