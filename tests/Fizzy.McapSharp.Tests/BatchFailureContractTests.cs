using System.Buffers.Binary;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class BatchFailureContractTests
{
    static byte[] Prefix()
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { UseChunks = false, Compression = McapCompression.None }, true))
        {
            var channel = writer.RegisterChannel("topic", "raw");
            writer.WriteMessage(new(channel, 1, 1, 1), [42]);
            writer.WriteMessage(new(channel, 2, 2, 2), [43]);
            writer.Complete();
        }
        var data = storage.ToArray();
        int messages = 0;
        for (int offset = 8; offset < data.Length - 8;)
        {
            if (data[offset] == 5 && ++messages == 2) return data[..(offset + 10)];
            offset += 9 + checked((int)BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset + 1)));
        }
        throw new InvalidOperationException("Missing second message.");
    }

    [Fact]
    public void CallerBuffersCanChangeBeforeBatchFailureAndSessionIsTerminal()
    {
        using var reader = McapFileReader.OpenMessages(new MemoryStream(Prefix()));
        var headers = new McapMessageHeader[3];
        var ranges = new McapPayloadRange[3];
        var payload = new byte[3];
        Assert.ThrowsAny<Exception>(() => reader.ReadBatch(headers, ranges, payload));
        // No count was published: even this written prefix must be discarded by callers.
        Assert.Equal(1U, headers[0].Sequence);
        Assert.Equal(42, payload[0]);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNext([], out _, out _));
    }

    [Fact]
    public void VisitorEffectsRemainButFailurePublishesNoLease()
    {
        int visited = 0;
        using (var reader = McapFileReader.OpenMessages(new MemoryStream(Prefix())))
        {
            bool Visit(in McapMessageHeader header, ReadOnlySpan<byte> data) { visited++; return true; }
            Assert.ThrowsAny<Exception>(() => reader.VisitMessages(Visit));
            Assert.Equal(1, visited);
            Assert.Throws<InvalidOperationException>(() => reader.ReadNext([], out _, out _));
        }
        using var leaseReader = McapFileReader.OpenMessages(new MemoryStream(Prefix()));
        McapMessageBatchLease? lease = null;
        Assert.ThrowsAny<Exception>(() => lease = leaseReader.ReadBatchLease());
        Assert.Null(lease);
        Assert.Throws<InvalidOperationException>(() => leaseReader.ReadBatchLease());
    }
}
