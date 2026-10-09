using System.Buffers.Binary;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class RawMessageOwnershipContractTests
{
    [Fact]
    public void MissingOwnedDeclarationConsumesMessageButLeavesRawCursorUsable()
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new()
        {
            UseChunks = false, CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            writer.WriteMessage(new(channel, 1, 1, 1), [11]);
            writer.WriteMessage(new(channel, 2, 2, 2), [22]);
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
        using var raw = new McapReadCursor(bytes, McapCursorMode.RawMessages);
        byte[] payload = new byte[1];
        Assert.Equal(McapReadStatus.Success, raw.ReadNext(payload, out var header, out _));
        Assert.Equal(ushort.MaxValue, header.ChannelId);
        Assert.Equal((byte)11, payload[0]);

        using var owned = new McapReadCursor(bytes, McapCursorMode.RawMessages);
        using (var messages = owned.ReadMessages().GetEnumerator())
            Assert.Equal(McapErrorKind.UnknownChannel, Assert.Throws<McapException>(() => messages.MoveNext()).Kind);
        var remaining = Assert.Single(owned.ReadMessages());
        Assert.Equal(2u, remaining.Sequence);
        Assert.Equal(new byte[] { 22 }, remaining.Data);
        Assert.Equal("topic", remaining.Channel.Topic);
        Assert.Equal(McapReadStatus.EndOfStream, owned.ReadNext(payload, out _, out _));
    }
}
