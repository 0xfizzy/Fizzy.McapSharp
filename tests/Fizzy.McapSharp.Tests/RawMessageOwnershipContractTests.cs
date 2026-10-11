using System.Buffers.Binary;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class RawMessageOwnershipContractTests
{
    [Fact]
    public void RawMessageDeclarationLookupIsExplicitAndNonterminal()
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new()
        {
            UseChunks = false, CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            writer.WriteToKnownChannel(new(channel, 1, 1, 1), [11]);
            writer.WriteToKnownChannel(new(channel, 2, 2, 2), [22]);
            writer.Complete();
        }
        var bytes = output.ToArray();
        for (int offset = 8; offset < bytes.Length - 8;)
        {
            int length = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(offset + 1)));
            if (bytes[offset] == (byte)McapOpcode.Message)
            {
                BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(offset + 9), ushort.MaxValue);
                break;
            }
            offset += 9 + length;
        }
        using var raw = new McapRawMessageReader(bytes);
        byte[] payload = new byte[1];
        Assert.Equal(McapReadStatus.Success, raw.ReadNext(payload, out var header, out _));
        Assert.Equal(ushort.MaxValue, header.ChannelId);
        Assert.Equal((byte)11, payload[0]);

        Assert.Equal(McapErrorKind.UnknownChannel, Assert.Throws<McapException>(() => raw.GetChannel(ushort.MaxValue)).Kind);
        Assert.Equal(McapReadStatus.Success, raw.ReadNext(payload, out var next, out _));
        Assert.Equal(2u, next.Sequence);
        Assert.Equal((byte)22, payload[0]);
        Assert.Equal("topic", raw.GetChannel(next.ChannelId).Topic);
        Assert.Null(typeof(McapRawMessageReader).GetMethod("ReadMessages"));
    }
}
