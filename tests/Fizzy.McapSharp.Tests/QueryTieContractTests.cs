using Xunit;

namespace Fizzy.McapSharp.Tests;

public class QueryTieContractTests
{
    // The second chunk starts earlier but contains a tie with the first chunk.
    // Upstream indexing can deliver its tied message first. A query guarantees
    // timestamp ordering and complete selection, not file-order ties.
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void OverlappingChunksGuaranteeTimeOrderAndCompleteSelection(McapCompression compression)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = compression,
            ChunkSize = null, CompressionThreads = 0 }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("selected", "raw");
            writer.WriteMessage(new(channel, 0, 10, 10), new byte[] { 0 });
            writer.Flush();
            writer.WriteMessage(new(channel, 1, 0, 0), new byte[] { 1 });
            writer.WriteMessage(new(channel, 2, 10, 10), new byte[] { 2 });
            writer.Flush();
            writer.Complete();
            Assert.Equal(2, writer.GetSummary().ChunkIndexes.Count);
        }
        var bytes = output.ToArray();
        foreach (var order in Enum.GetValues<McapReadOrder>())
        foreach (bool indexed in new[] { false, true })
        {
            using Stream input = indexed ? new MemoryStream(bytes) : new NonSeekable(bytes);
            using var reader = McapReaderFactory.OpenMessages(input,
                new() { Order = order, AllowBufferedSort = !indexed });
            var messages = reader.ReadMessages().ToArray();
            Assert.Equal(new uint[] { 0, 1, 2 }, messages.Select(m => m.Sequence).Order());
            Assert.All(messages, message => Assert.Equal(new byte[] { (byte)message.Sequence }, message.Data));
            if (order == McapReadOrder.File)
                Assert.Equal(new uint[] { 0, 1, 2 }, messages.Select(m => m.Sequence));
            else
                Assert.Equal(order == McapReadOrder.LogTime ? new ulong[] { 0, 10, 10 } : [10ul, 10, 0],
                    messages.Select(m => m.LogTime));
        }
    }

    sealed class NonSeekable(byte[] bytes) : MemoryStream(bytes)
    {
        public override bool CanSeek => false;
    }
}
