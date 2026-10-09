using System.Text.Json;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public sealed class WriterSummaryTests
{
    [Theory]
    [InlineData(McapCompression.None, true)]
    [InlineData(McapCompression.Lz4, true)]
    [InlineData(McapCompression.Zstd, true)]
    [InlineData(McapCompression.None, false)]
    [InlineData(McapCompression.Lz4, false)]
    [InlineData(McapCompression.Zstd, false)]
    public void SummaryIsIndependentAndCursorOutlivesWriter(McapCompression compression, bool emitSummary)
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        try
        {
            using var writer = new McapWriter(path, new() { Compression = compression, ChunkSize = 1024,
                EmitSummaryRecords = emitSummary });
            var schema = writer.RegisterSchema("schema\"雪", "raw", [1, 2, 3]);
            var a = writer.RegisterChannel("a", "raw", schema);
            var b = writer.RegisterChannel("b", "raw");
            byte[] payload = new byte[2048];
            for (uint i = 0; i < 16; i++) writer.WriteMessage(new(i % 2 == 0 ? a : b, i, i, ulong.MaxValue), payload);
            writer.WriteAttachment("name\"雪", "raw", ulong.MaxValue, 0, [4, 5]);
            writer.WriteMetadata("meta\n雪", new Dictionary<string, string> { ["k"] = "v" });
            writer.Complete();
            writer.Complete();
            var first = writer.GetSummary();
            string expected = JsonSerializer.Serialize(first);
            Assert.Equal(16ul, first.Statistics!.MessageCount);
            Assert.Equal(2, first.ChannelIds.Count);
            Assert.Single(first.SchemaIds);
            Assert.True(first.ChunkIndexes.Count >= 2);
            Assert.Equal(emitSummary ? 1 : 0, first.AttachmentIndexes.Count);
            Assert.Equal(emitSummary ? 1 : 0, first.MetadataIndexes.Count);
            if (emitSummary)
            {
                Assert.Equal("name\"雪", first.AttachmentIndexes[0].Name);
                Assert.Equal(ulong.MaxValue, first.AttachmentIndexes[0].LogTime);
                Assert.Equal("meta\n雪", first.MetadataIndexes[0].Name);
            }
            var independent = writer.GetSummary();
            Assert.Equal(expected, JsonSerializer.Serialize(independent));
            var counts = Assert.IsAssignableFrom<IDictionary<ushort, ulong>>(first.Statistics.ChannelMessageCounts);
            counts[a] = 999;
            Assert.Equal(expected, JsonSerializer.Serialize(writer.GetSummary()));
            Assert.Equal(expected, JsonSerializer.Serialize(independent));
            using var cursor = writer.OpenSummaryRecords();
            writer.Dispose();
            Assert.NotEmpty(cursor.ReadRecords());
            Assert.Equal(expected, JsonSerializer.Serialize(independent));
            new McapReaderFactory(path).Validate();
            if (emitSummary)
            {
                using var reader = new McapReaderFactory(path).OpenIndexedMessages();
                Assert.Equal(16, reader.ReadMessages().Count());
            }
            else Assert.Equal(16, new McapReaderFactory(path).ReadMessages().Count());
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void EmptySummaryCanBeRequestedRepeatedly()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, leaveOpen: true);
        writer.Complete();
        Assert.Equal(0ul, writer.GetSummary().Statistics!.MessageCount);
        Assert.Empty(writer.GetSummary().ChunkIndexes);
        Assert.Same(stream, writer.IntoInner());
    }
}
