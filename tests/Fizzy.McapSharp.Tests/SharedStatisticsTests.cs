using Xunit;
namespace Fizzy.McapSharp.Tests;

public sealed class SharedStatisticsTests
{
    [Fact]
    public void IndexedMetadataAndAttachmentChargeOnlyActualCallerDelivery()
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { UseChunks = false }, true))
        {
            writer.WriteMetadata("metadata", new Dictionary<string, string> { ["key"] = "value" });
            writer.WriteAttachment("attachment", "raw", 4, 5, new byte[1024 * 1024]);
            writer.Complete();
        }
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using (var snapshot = new McapIndexSnapshot(stream.ToArray(), new() { Budget = budget }))
        {
            var summary = snapshot.GetSummary()!;
            var before = budget.GetDetailedStatistics();
            snapshot.ReadMetadata(summary.MetadataIndexes[0], Span<byte>.Empty, out var metadataLength);
            snapshot.ReadAttachment(summary.AttachmentIndexes[0], Span<byte>.Empty, out var attachmentLength);
            Assert.Equal(before.Flow.DeliveryCopyBytes, budget.GetDetailedStatistics().Flow.DeliveryCopyBytes);
            var metadata = new byte[checked((int)metadataLength)];
            var attachment = new byte[checked((int)attachmentLength)];
            snapshot.ReadMetadata(summary.MetadataIndexes[0], metadata, out var metadataWritten);
            snapshot.ReadAttachment(summary.AttachmentIndexes[0], attachment, out var attachmentWritten);
            Assert.Equal(metadataLength, metadataWritten);
            Assert.Equal(attachmentLength, attachmentWritten);
            var after = budget.GetDetailedStatistics();
            Assert.Equal(before.Flow.DeliveryCopyBytes + metadataLength + attachmentLength, after.Flow.DeliveryCopyBytes);
            Assert.Equal(before.Flow.InputCopyBytes, after.Flow.InputCopyBytes);
            Assert.Equal(before.CurrentBytes, after.CurrentBytes);
        }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void SharedSummaryControlRemainsChargedUntilLastCursor()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var stream = new MemoryStream();
        McapBufferReader first;
        McapBufferReader second;
        ulong declarationBytes;
        using (var writer = new McapWriter(stream, new()
        {
            UseChunks = false, Compression = McapCompression.None, Memory = new() { Budget = budget }
        }, true))
        {
            writer.Complete();
            // No Schema/Channel declarations: this is the retained summary control.
            declarationBytes = budget.GetDetailedStatistics().Declaration.LiveBytes;
            Assert.True(declarationBytes > 0);
            first = writer.OpenSummaryRecords();
            second = writer.OpenSummaryRecords();
            Assert.Equal(declarationBytes, budget.GetDetailedStatistics().Declaration.LiveBytes);
        }
        using (second)
        {
            first.Dispose();
            Assert.Equal(declarationBytes, budget.GetDetailedStatistics().Declaration.LiveBytes);
            Assert.Contains(second.ReadRecords(), record => record.Opcode == 11);
        }
        BudgetAssertions.Idle(budget);

        using (var snapshot = new McapIndexSnapshot(stream.ToArray(), new() { Budget = budget }))
        {
            declarationBytes = budget.GetDetailedStatistics().Declaration.LiveBytes;
            Assert.True(declarationBytes > 0);
            first = snapshot.OpenSummaryRecords();
            second = snapshot.OpenSummaryRecords();
            Assert.Equal(declarationBytes, budget.GetDetailedStatistics().Declaration.LiveBytes);
        }
        using (second)
        {
            first.Dispose();
            Assert.Equal(declarationBytes, budget.GetDetailedStatistics().Declaration.LiveBytes);
            Assert.Contains(second.ReadRecords(), record => record.Opcode == 11);
        }
        BudgetAssertions.Idle(budget);
    }
    [Theory]
    [InlineData(McapCompression.None, false)] [InlineData(McapCompression.None, true)]
    [InlineData(McapCompression.Lz4, false)] [InlineData(McapCompression.Lz4, true)]
    [InlineData(McapCompression.Zstd, false)] [InlineData(McapCompression.Zstd, true)]
    public void ReaderDeclarationsHaveExactStorageAndIndependentOwnedExports(McapCompression compression, bool buffer)
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var stream = new MemoryStream();
        var fields = Enumerable.Range(0, 2000).ToDictionary(i => $"key/{i:000000}", i => $"value/{i}");
        ushort channel;
        using (var writer = new McapWriter(stream, new() { Compression = compression }, true))
        {
            var schema = writer.RegisterSchema("名字", "raw", new byte[100_000]);
            channel = writer.RegisterChannel("topic", "raw", schema, fields);
            writer.WriteMessage(new(channel, 0, 0, 0), "data"u8);
            writer.Complete();
        }
        if (buffer)
        {
            using var reader = new McapBufferReader(stream.ToArray(), McapBufferReadMode.Messages, false, new() { Budget = budget });
            Assert.Single(reader.ReadMessages());
            Check(reader.GetChannel(channel), reader.GetChannel(channel));
        }
        else
        {
            stream.Position = 0;
            using var reader = McapReader.OpenMessages(stream, new() { Order = McapReadOrder.File }, true,
                new() { Memory = new() { Budget = budget } });
            // Shared summary declarations are then compared against raw sequential declarations.
            Assert.NotNull(reader.GetSummary());
            Assert.Single(reader.ReadMessages());
            Check(reader.GetChannel(channel), reader.GetChannel(channel));
        }
        BudgetAssertions.Idle(budget);
        void Check(McapChannel first, McapChannel second)
        {
            Assert.Equal(0UL, budget.GetDetailedStatistics().Declaration.ReservedBytes);
            Assert.True(budget.GetDetailedStatistics().Declaration.LiveBytes > 100_000);
            Assert.Equal(2000, first.Metadata.Count);
            Assert.Equal("value/1999", first.Metadata["key/001999"]);
            Assert.Equal("名字", first.Schema!.Name);
            Assert.Equal(100_000, first.Schema.Data.Length);
            first.Schema.Data[0] = 99;
            Assert.Equal(0, second.Schema!.Data[0]);
        }
    }

    [Fact]
    public void PagedDeclarationsOutliveWriterAndOwnedSchemaCopiesRemainIndependent()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        var fields = Enumerable.Range(0, 2000).ToDictionary(i => $"key/{i:000000}", i => $"value/{i}");
        using var stream = new MemoryStream();
        McapBufferReader cursor;
        ushort channel;
        using (var writer = new McapWriter(stream, new()
        {
            UseChunks = false, Compression = McapCompression.None, Memory = new() { Budget = budget }
        }, true))
        {
            var schema = writer.RegisterSchema("名字", "raw", new byte[100_000]);
            channel = writer.RegisterChannel("topic", "raw", schema, fields);
            writer.Complete();
            Assert.Equal(0UL, budget.GetDetailedStatistics().Declaration.ReservedBytes);
            cursor = writer.OpenSummaryRecords();
        }
        using (cursor)
        {
            Check(cursor.GetChannel(channel));
            var first = cursor.GetChannel(channel);
            first.Schema!.Data[0] = 99;
            Assert.Equal(0, cursor.GetChannel(channel).Schema!.Data[0]);
            Assert.Contains(cursor.ReadRecords(), record => record.Opcode == 3);
        }
        BudgetAssertions.Idle(budget);
        using (var snapshot = new McapIndexSnapshot(stream.ToArray(), new() { Budget = budget }))
        {
            using var readSummary = snapshot.OpenSummaryRecords();
            Check(readSummary.GetChannel(channel));
            Assert.Equal(0UL, budget.GetDetailedStatistics().Declaration.ReservedBytes);
        }
        BudgetAssertions.Idle(budget);
        static void Check(McapChannel value)
        {
            Assert.Equal("topic", value.Topic);
            Assert.Equal(2000, value.Metadata.Count);
            Assert.Equal("value/1999", value.Metadata["key/001999"]);
            Assert.Equal("名字", value.Schema!.Name);
            Assert.Equal(100_000, value.Schema.Data.Length);
        }
    }

    [Theory]
    [InlineData(true, true)]
    [InlineData(true, false)]
    [InlineData(false, true)]
    public void AttachmentHeaderStorageSurvivesPagedSummaryAndSequentialScan(bool indexes, bool crc)
    {
        const int count = 5000;
        const string name = "附件/名字";
        const string media = "application/数据";
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new()
        {
            Compression = McapCompression.None, UseChunks = false,
            EmitAttachmentIndexes = indexes, CalculateAttachmentCrcs = crc,
            Memory = new() { Budget = budget }
        }, true))
        {
            for (var i = 0; i < count; ++i)
            {
                if (i % 2 == 0) writer.WriteAttachment(name, media, 7, 8, [1, 2, 3]);
                else
                {
                    writer.StartAttachment(name, media, 7, 8, 3);
                    writer.WriteAttachmentBytes([1]);
                    writer.WriteAttachmentBytes([2, 3]);
                    writer.FinishAttachment();
                }
            }
            writer.Complete();
            Check(writer.GetSummary());
            Check(writer.GetSummary());
        }
        BudgetAssertions.Idle(budget);
        storage.Position = 0;
        using var reader = McapReader.OpenMessages(storage, leaveOpen: true);
        Check(reader.GetSummary()!);
        Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext(Span<byte>.Empty, out _, out _));
        Check(reader.GetSummary()!);
        void Check(McapSummary summary)
        {
            Assert.Equal((uint)count, summary.Statistics!.AttachmentCount);
            Assert.Equal(indexes ? count : 0, summary.AttachmentIndexes.Count);
            Assert.All(summary.AttachmentIndexes, index =>
            {
                Assert.Equal(name, index.Name); Assert.Equal(media, index.MediaType);
                Assert.Equal(3UL, index.DataSize); Assert.Equal(7UL, index.LogTime); Assert.Equal(8UL, index.CreateTime);
            });
            if (indexes) Assert.True(summary.AttachmentIndexes[^1].Offset > summary.AttachmentIndexes[0].Offset);
        }
    }

    [Fact]
    public void PagedMetadataNamesSurviveSummarySharingAndSequentialScan()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var storage = new MemoryStream();
        const int count = 5000;
        const string name = "metadata/名字";
        var fields = new Dictionary<string, string>();
        using (var writer = new McapWriter(storage, new()
        {
            Compression = McapCompression.None, UseChunks = false,
            Memory = new() { Budget = budget }
        }, true))
        {
            for (var i = 0; i < count; ++i) writer.WriteMetadata(name, fields);
            writer.Complete();
            Check(writer.GetSummary());
            Check(writer.GetSummary());
            Assert.True(budget.GetDetailedStatistics().Index.LiveBytes > (ulong)(count * name.Length));
        }
        BudgetAssertions.Idle(budget);
        storage.Position = 0;
        using var reader = McapReader.OpenMessages(storage, leaveOpen: true);
        Check(reader.GetSummary()!);
        Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext(Span<byte>.Empty, out _, out _));
        Check(reader.GetSummary()!);
        static void Check(McapSummary summary)
        {
            Assert.Equal((uint)count, summary.Statistics!.MetadataCount);
            Assert.Equal(count, summary.MetadataIndexes.Count);
            Assert.All(summary.MetadataIndexes, index => Assert.Equal(name, index.Name));
            Assert.True(summary.MetadataIndexes[^1].Offset > summary.MetadataIndexes[0].Offset);
        }
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void CrossPageCountsSurviveCompletionRepeatedExportAndSequentialScan(McapCompression compression)
    {
        using var storage = new MemoryStream();
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        ushort[] ids = [1, 256, 512, ushort.MaxValue];
        using (var writer = new McapWriter(storage, new()
        {
            Compression = compression, ChunkSize = 128,
            Memory = new() { Budget = budget }
        }, true))
        {
            for (var i = 0; i < ids.Length; ++i)
            {
                writer.RegisterChannel(ids[i], $"topic/{i}", "raw");
                for (uint sequence = 0; sequence <= i; ++sequence)
                    writer.WriteMessage(new McapMessageHeader(ids[i], sequence, sequence, sequence), "data"u8);
            }
            writer.Complete();
            for (var attempt = 0; attempt < 3; ++attempt)
                Check(writer.GetSummary());
        }
        BudgetAssertions.Idle(budget);
        storage.Position = 0;
        using (var reader = McapReader.OpenMessages(storage, leaveOpen: true))
        {
            Check(reader.GetSummary()!);
            Span<byte> data = stackalloc byte[4];
            var count = 0;
            while (reader.ReadNext(data, out _, out _) == McapReadStatus.Message)
                ++count;
            Assert.Equal(10, count);
            Check(reader.GetSummary()!);
            Check(reader.GetSummary()!);
        }
        void Check(McapSummary summary)
        {
            Assert.NotEmpty(summary.ChunkIndexes);
            Assert.Equal((uint)summary.ChunkIndexes.Count, summary.Statistics!.ChunkCount);
            var indexedChannels = summary.ChunkIndexes.SelectMany(index => index.MessageIndexOffsets.Keys).Distinct().Order().ToArray();
            Assert.Equal(ids.Order().ToArray(), indexedChannels);
            Assert.All(summary.ChunkIndexes, index =>
            {
                Assert.Equal(compression == McapCompression.None ? "" : compression.ToString().ToLowerInvariant(), index.Compression);
                Assert.True(index.ChunkLength > 0);
                Assert.All(index.MessageIndexOffsets.Values, offset => Assert.True(offset >= index.ChunkStartOffset + index.ChunkLength));
            });
            Assert.Equal(10UL, summary.Statistics!.MessageCount);
            Assert.Equal(4, summary.Statistics.ChannelMessageCounts.Count);
            for (var i = 0; i < ids.Length; ++i)
                Assert.Equal((ulong)i + 1, summary.Statistics.ChannelMessageCounts[ids[i]]);
        }
    }
}
