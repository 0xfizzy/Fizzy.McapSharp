using Xunit;

namespace Fizzy.McapSharp.Tests;

public class PreparedChannelBudgetTests
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void SnapshotKeepsItsDomainAndSurvivesCallerMutation(McapCompression compression)
    {
        var source = new McapMemoryBudget(4 * 1024 * 1024, 65536, 0);
        var payload = new byte[] { 1, 2, 3, 4 };
        var metadata = Enumerable.Range(0, 1000).ToDictionary(i => $"key{i:D4}", i => $"value{i}");
        var channel = new McapChannel(9, "/topic", "raw", new(7, "schema", "raw", payload), metadata);
        using var prepared = new McapPreparedChannel(channel, source);
        var snapshot = source.GetDetailedStatistics();
        Assert.True(snapshot.Declaration.LiveBytes > 0);
        Assert.Equal(snapshot.CurrentBytes - BudgetAssertions.ControlBytes, snapshot.Declaration.LiveBytes);
        Assert.Equal(0UL, snapshot.Declaration.ReservedBytes);
        payload[0] = 99;
        metadata["key0000"] = "changed";
        for (int run = 0; run < 2; run++)
        {
            var destination = new McapMemoryBudget(maxBlockBytes: 65536, maxRetainedBytes: 0);
            using var output = new MemoryStream();
            using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 4096, Memory = new() { Budget = destination } }, true))
            {
                writer.WriteMessage(prepared, new(9, 1, 2, 3), "test"u8);
                writer.WriteMessage(prepared, new(9, 2, 3, 4), "again"u8);
                writer.Complete();
            }
            BudgetAssertions.Idle(destination);
            Assert.Equal(snapshot, source.GetDetailedStatistics());
            using var reader = McapReader.OpenMessages(new MemoryStream(output.ToArray()), options: McapReaderOptions.Strict);
            var messages = reader.ReadMessages().ToArray();
            Assert.Equal(2, messages.Length);
            var message = messages[0];
            Assert.Equal("value0", message.Channel.Metadata["key0000"]);
            Assert.Equal(new byte[] { 1, 2, 3, 4 }, message.Channel.Schema!.Data);
        }
        prepared.Dispose();
        prepared.Dispose();
        BudgetAssertions.Idle(source);
    }

    [Fact]
    public void WriterRetainsItsOwnDeclarationsAfterSnapshotDisposal()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        var channel = new McapChannel(1, "topic", "raw", new(2, "schema", "raw", [1, 2]), new Dictionary<string, string> { ["key"] = "value" });
        var prepared = new McapPreparedChannel(channel, budget);
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = McapCompression.None, ChunkSize = 32 }, true))
        {
            writer.WriteMessage(prepared, new(1, 0, 0, 0), "first"u8);
            prepared.Dispose();
            BudgetAssertions.Idle(budget);
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
    public void RefusedSnapshotReleasesPartialStorageAndEnforcesSchemaBlockLimit()
    {
        var channel = new McapChannel(1, "topic", "raw", new(2, "schema", "raw", new byte[4096]), new Dictionary<string, string>());
        var probe = new McapMemoryBudget(1024 * 1024, 4096, 0);
        using (var prepared = new McapPreparedChannel(channel, probe)) { }
        var peak = probe.GetStatistics().PeakBytes;
        var limited = new McapMemoryBudget(peak - 1, Math.Min(4096UL, peak - 1), 0);
        Assert.ThrowsAny<McapException>(() => new McapPreparedChannel(channel, limited));
        BudgetAssertions.Idle(limited);
        var smallBlock = new McapMemoryBudget(1024 * 1024, 1024, 0);
        Assert.ThrowsAny<McapException>(() => new McapPreparedChannel(channel, smallBlock));
        BudgetAssertions.Idle(smallBlock);
        Assert.Throws<ArgumentNullException>(() => new McapPreparedChannel(channel, null!));
    }
}
