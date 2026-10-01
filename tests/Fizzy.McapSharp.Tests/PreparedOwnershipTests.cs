using Xunit;
namespace Fizzy.McapSharp.Tests;
public class PreparedOwnershipTests
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void PreparedChannelSurvivesCallerMutation(McapCompression compression)
    {
        var payload = new byte[] { 1, 2, 3, 4 };
        var metadata = Enumerable.Range(0, 1000).ToDictionary(i => $"key{i:D4}", i => $"value{i}");
        var channel = new McapChannel(9, "/topic", "raw", new(7, "schema", "raw", payload), metadata);
        using var prepared = new McapPreparedChannel(channel);
        payload[0] = 99;
        metadata["key0000"] = "changed";
        for (int run = 0; run < 2; run++)
        {
            using var output = new MemoryStream();
            using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 4096 }, true))
            {
                writer.WriteMessage(prepared, new(9, 1, 2, 3), "test"u8);
                writer.WriteMessage(prepared, new(9, 2, 3, 4), "again"u8);
                writer.Complete();
            }
            using var reader = McapReader.OpenMessages(new MemoryStream(output.ToArray()), options: McapReaderOptions.Strict);
            var messages = reader.ReadMessages().ToArray();
            Assert.Equal(2, messages.Length);
            var message = messages[0];
            Assert.Equal("value0", message.Channel.Metadata["key0000"]);
            Assert.Equal(new byte[] { 1, 2, 3, 4 }, message.Channel.Schema!.Data);
        }
        prepared.Dispose();
        prepared.Dispose();
    }

    [Fact]
    public void WriterRetainsItsOwnDeclarationsAfterSnapshotDisposal()
    {
        var channel = new McapChannel(1, "topic", "raw", new(2, "schema", "raw", [1, 2]), new Dictionary<string, string> { ["key"] = "value" });
        var prepared = new McapPreparedChannel(channel);
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = McapCompression.None, ChunkSize = 32 }, true))
        {
            writer.WriteMessage(prepared, new(1, 0, 0, 0), "first"u8);
            prepared.Dispose();
            writer.Flush();
            writer.WriteMessage(new(1, 1, 1, 1), "second"u8);
            writer.Complete();
        }
        output.Position = 0;
        using var reader = McapReader.OpenMessages(output, leaveOpen: true, options: McapReaderOptions.Strict);
        var messages = reader.ReadMessages().ToArray();
        Assert.Equal(2, messages.Length);
        Assert.All(messages, message => {
            Assert.Equal(new byte[] { 1, 2 }, message.Channel.Schema!.Data);
            Assert.Equal("value", message.Channel.Metadata["key"]);
        });
    }

    [Fact]
    public void PartialSummaryScanInheritsRecordLengthLimit()
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new()
        {
            UseChunks = false, Compression = McapCompression.None,
            RepeatSchemas = false, RepeatChannels = true
        }, true))
        {
            var schema = writer.RegisterSchema("schema", "raw", [1]);
            writer.RegisterChannel("topic", "raw", schema);
            writer.WritePrivateRecord(0x80, new byte[256 * 1024]);
            writer.Complete();
        }
        stream.Position = 0;
        using (var reader = McapReader.OpenRecords(stream, leaveOpen: true, options: new()
        {
            RecordLengthLimit = 128 * 1024
        }))
        {
            var position = stream.Position;
            var error = Assert.Throws<McapException>(() => reader.GetSummary());
            Assert.Equal(McapErrorKind.RecordTooLarge, error.Kind);
            Assert.Equal(position, stream.Position);
        }
        stream.Position = 0;
        using var unrestricted = McapReader.OpenRecords(stream, leaveOpen: true);
        Assert.Empty(unrestricted.GetSummary()!.SchemaIds);
    }

    [Fact]
    public void PreparedMetadataPreservesLargeSnapshot()
    {
        var values = Enumerable.Range(0, 1000).Reverse().ToDictionary(i => $"key{i:D4}", i => $"值/{i}/🚀");
        using (var operation = McapPreparedOperation.Metadata("元数据", values))
        {
            values["key0042"] = "changed";
            using var stream = new MemoryStream();
            using (var writer = new McapWriter(stream, new() { Compression = McapCompression.None }, true))
            {
                writer.WritePrepared(operation);
                writer.WritePrepared(operation);
                writer.Complete();
                Assert.Equal(2, writer.GetSummary().MetadataIndexes.Count);
            }
            stream.Position = 0;
            using var reader = McapReader.OpenRecords(stream, leaveOpen: true);
            var metadata = reader.ReadMetadata().ToArray();
            Assert.Equal(2, metadata.Length);
            foreach (var item in metadata)
            {
                Assert.Equal("元数据", item.Name);
                Assert.Equal(1000, item.Values.Count);
                Assert.Equal("值/42/🚀", item.Values["key0042"]);
            }
        }
    }
}
