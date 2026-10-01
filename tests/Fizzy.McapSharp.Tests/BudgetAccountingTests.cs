using Xunit;
namespace Fizzy.McapSharp.Tests;
public class BudgetAccountingTests
{
    [Fact]
    public void BudgetControlStorageIsLiveAndTooSmallDomainsAreRejected()
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        var statistics = budget.GetDetailedStatistics();
        Assert.True(statistics.CurrentBytes > 0);
        Assert.Equal(statistics.CurrentBytes, statistics.Scratch.LiveBytes);
        Assert.Equal(statistics.CurrentBytes, statistics.AllocatedBytes);
        Assert.Equal(0UL, statistics.Scratch.ReservedBytes);
        var limit = statistics.CurrentBytes - 1;
        var error = Assert.Throws<McapException>(() => new McapMemoryBudget(limit, 1, 0));
        Assert.Equal("NativeDomain", error.Details.GetProperty("resource").GetString());
        Assert.Equal(statistics.CurrentBytes, error.Details.GetProperty("requested").GetUInt64());
        var exact = new McapMemoryBudget(statistics.CurrentBytes, 1, 0);
        BudgetAssertions.Idle(exact);
    }
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void PartialSummaryScanInheritsReaderLimits(bool recordLimit)
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
        var budget = new McapMemoryBudget(8 * 1024 * 1024, recordLimit ? 1024UL * 1024 : 64UL * 1024, 0);
        using (var reader = McapReader.OpenRecords(stream, leaveOpen: true, options: new()
        {
            Memory = new() { Budget = budget },
            RecordLengthLimit = recordLimit ? 128 * 1024 : null
        }))
        {
            var position = stream.Position;
            var error = Assert.Throws<McapException>(() => reader.GetSummary());
            if (recordLimit) Assert.Equal(McapErrorKind.RecordTooLarge, error.Kind);
            else Assert.Contains("StorageBlock", error.Message);
            Assert.Equal(position, stream.Position);
            Assert.True(budget.GetStatistics().PeakBytes <= budget.MaxBytes);
        }
        BudgetAssertions.Idle(budget);
        stream.Position = 0;
        using var unrestricted = McapReader.OpenRecords(stream, leaveOpen: true);
        Assert.Empty(unrestricted.GetSummary()!.SchemaIds);
    }

    [Theory]
    [InlineData(0, "", 3UL)]
    [InlineData(3, "zstd", 4UL)]
    [InlineData(5, "zstd", 9UL)]
    public void PreparedIndexNestedStorageIsExactlyLive(int count, string compression, ulong allocations)
    {
        ushort[] keys = [0, 255, 256, 512, ushort.MaxValue];
        var offsets = keys.Take(count).ToDictionary(key => key, key => (ulong)key);
        var index = new McapChunkIndex(1, 2, 3, 4, offsets, 0, compression, 0, 0);
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        ulong peak;
        using (var prepared = new McapPreparedChunkIndex(index, budget))
        {
            var stats = budget.GetDetailedStatistics();
            Assert.Equal(allocations + BudgetAssertions.ControlAllocations, stats.AllocationCount);
            Assert.True(stats.Index.LiveBytes > 0);
            Assert.Equal(0UL, stats.Index.ReservedBytes);
            Assert.True(stats.Flow.OtherCopyBytes > 0);
            CheckTotals(budget);
            peak = budget.GetStatistics().PeakBytes;
        }
        BudgetAssertions.Idle(budget);
        var limited = new McapMemoryBudget(peak - 1, peak - 1, 0);
        Assert.ThrowsAny<McapException>(() => new McapPreparedChunkIndex(index, limited));
        BudgetAssertions.Idle(limited);
    }

    [Fact]
    public void RefusedEngineRootDoesNotConsumeCallerInput()
    {
        var budget = new McapMemoryBudget(BudgetAssertions.ControlBytes, 1, 0);
        using var stream = new MemoryStream(new byte[] { 1, 2, 3 });
        Assert.ThrowsAny<McapException>(() => new McapAsyncReader(stream, new() { Memory = new() { Budget = budget } }, true));
        Assert.Equal(0, stream.Position);
        Assert.True(stream.CanRead);
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void RefusedWriterRootDoesNotCreateFileOrAdvanceStream()
    {
        var budget = new McapMemoryBudget(BudgetAssertions.ControlBytes, 1, 0);
        var options = new McapWriterOptions { Memory = new() { Budget = budget } };
        using var stream = new MemoryStream();
        stream.WriteByte(42);
        Assert.ThrowsAny<McapException>(() => new McapWriter(stream, options, true));
        Assert.Equal(1, stream.Position);
        Assert.Equal(new byte[] { 42 }, stream.ToArray());
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        try
        {
            Assert.ThrowsAny<McapException>(() => new McapWriter(path, options));
            Assert.False(File.Exists(path));
            BudgetAssertions.Idle(budget);
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void PrivateStatisticsDtosHaveStableAbiLayout()
    {
        Assert.Equal(40,System.Runtime.InteropServices.Marshal.SizeOf<NativeBudgetStatistics>());
        Assert.Equal(32,System.Runtime.InteropServices.Marshal.SizeOf<NativeResourceStatistics>());
        Assert.Equal(312, System.Runtime.InteropServices.Marshal.OffsetOf<NativeDetailedBudgetStatistics>(nameof(NativeDetailedBudgetStatistics.Flow)).ToInt32());
        Assert.Equal(424, System.Runtime.InteropServices.Marshal.OffsetOf<NativeDetailedBudgetStatistics>(nameof(NativeDetailedBudgetStatistics.ActiveLeasePayloadBytes)).ToInt32());
        Assert.Equal(112,System.Runtime.InteropServices.Marshal.SizeOf<NativeBudgetFlowStatistics>());
        Assert.Equal(488,System.Runtime.InteropServices.Marshal.SizeOf<NativeDetailedBudgetStatistics>());
        Assert.Equal(480, System.Runtime.InteropServices.Marshal.OffsetOf<NativeDetailedBudgetStatistics>(nameof(NativeDetailedBudgetStatistics.MappedLogicalBytes)).ToInt32());
    }
    [Fact]
    public void StatisticsConversionPreservesEveryFieldAndPublicDeconstruction()
    {
        static NativeResourceStatistics Resource(ulong n) => new() { CurrentBytes=n, PeakBytes=n+1, LiveBytes=n+2, ReservedBytes=n+3 };
        var wire = new NativeDetailedBudgetStatistics {
            Input=Resource(1), Decompressed=Resource(11), Writer=Resource(21),
            CodecEncoder=Resource(31), CodecDecoder=Resource(41), Index=Resource(51),
            Descriptor=Resource(61), Declaration=Resource(71), Scratch=Resource(81),
            AllocationCount=91, AllocatedBytes=92, BudgetRejections=93,
            Flow=new() { InputCopyBytes=101, CompactionCopyBytes=102, DeliveryCopyBytes=103, OtherCopyBytes=104,
                EncodedInputBytes=105, EncodedOutputBytes=106, DecodedInputBytes=107, DecodedOutputBytes=108,
                DecompressionsStarted=109, DecompressionsCompleted=110, CacheHits=111, CacheMisses=112, CacheEvictions=113 },
            ActiveLeasePayloadBytes=121, CachedPayloadBytes=122, CurrentBytes=123, PeakBytes=124, IdleBytes=125
        };
        var expectedFlow = new McapBudgetFlowStatistics(101,102,103,104,105,106,107,108,109,110,111,112,113);
        var expected = new McapDetailedBudgetStatistics(
            new(1,2,3,4), new(11,12,13,14), new(21,22,23,24), new(31,32,33,34),
            new(41,42,43,44), new(51,52,53,54), new(61,62,63,64), new(71,72,73,74), new(81,82,83,84),
            91,92,93,expectedFlow,121,122,123,124,125);
        var actual = wire.ToPublic();
        Assert.Equal(expected, actual);
        var (input, decompressed, writer, encoder, decoder, index, descriptor, declaration, scratch,
            allocations, allocated, rejected, flow, lease, cache, current, peak, idle) = actual;
        Assert.Equal(expected, new McapDetailedBudgetStatistics(input,decompressed,writer,encoder,decoder,index,
            descriptor,declaration,scratch,allocations,allocated,rejected,flow,lease,cache,current,peak,idle));
        var (a,b,c,d,e,f,g,h,i,j,k,l,m) = flow;
        Assert.Equal(expectedFlow, new McapBudgetFlowStatistics(a,b,c,d,e,f,g,h,i,j,k,l,m));
        Assert.Equal(new McapBudgetStatistics(1,2,3,4,5),
            new NativeBudgetStatistics { CurrentBytes=1,PeakBytes=2,RetainedBytes=3,AllocationCount=4,StorageCopyBytes=5 }.ToPublic());
    }
    [Fact]
    public void AddedStatisticsPropertiesAreConvertedWithoutChangingConstructors()
    {
        var snapshot = new NativeDetailedBudgetStatistics {
            ReallocationCount=201, ImmediatelyReclaimableBytes=202, MappedLogicalBytes=203,
            Flow=new() { ReclaimedBytes=204 }
        }.ToPublic();
        Assert.Equal(201UL,snapshot.ReallocationCount);
        Assert.Equal(202UL,snapshot.ImmediatelyReclaimableBytes);
        Assert.Equal(203UL,snapshot.MappedLogicalBytes);
        Assert.Equal(204UL,snapshot.Flow.ReclaimedBytes);
        Assert.False(typeof(McapDetailedBudgetStatistics).GetProperty(nameof(snapshot.ReallocationCount))!.SetMethod!.IsPublic);
        Assert.False(typeof(McapBudgetFlowStatistics).GetProperty(nameof(snapshot.Flow.ReclaimedBytes))!.SetMethod!.IsPublic);
        Assert.Equal(0UL,new McapBudgetFlowStatistics(1,2,3,4,5,6,7,8,9,10,11,12,13).ReclaimedBytes);
    }
    [Theory]
    [InlineData(McapCompression.Lz4, 0)]
    [InlineData(McapCompression.Zstd, 0)]
    [InlineData(McapCompression.Zstd, 1)]
    [InlineData(McapCompression.Zstd, 2)]
    public void CodecAllocationsAreChargedAndReleased(McapCompression compression, uint threads)
    {
        var budget = new McapMemoryBudget(maxRetainedBytes: 0);
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, CompressionThreads = threads, ChunkSize = 8 * 1024 * 1024, Memory = new() { Budget = budget } }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            writer.WriteMessage(new(channel, 1, 1, 0), new byte[1024 * 1024]);
            var detail = budget.GetDetailedStatistics();
            Assert.True(detail.CodecEncoder.LiveBytes > 0);
            CheckTotals(budget);
            writer.Complete();
        }
        BudgetAssertions.Idle(budget);
        stream.Position = 0;
        using (var reader = McapReader.OpenMessages(stream, new() { Order = McapReadOrder.File }, true, new() { Memory = new() { Budget = budget } }))
        {
            using var batch = reader.ReadBatchLease(1)!;
            Assert.True(budget.GetDetailedStatistics().CodecDecoder.LiveBytes > 0);
            Assert.Equal(1024 * 1024, batch.GetPayload(0).Length);
            var leased=budget.GetDetailedStatistics().ActiveLeasePayloadBytes;
            Assert.True(leased>=1024*1024);
            using(var retained=batch.RetainMessage(0)) Assert.Equal(leased,budget.GetDetailedStatistics().ActiveLeasePayloadBytes);
            var flow=budget.GetDetailedStatistics().Flow;
            Assert.True(flow.DecompressionsStarted>0);
            Assert.True(flow.DecodedOutputBytes>=1024*1024);
            CheckTotals(budget);
        }
        BudgetAssertions.Idle(budget);
        Assert.Equal(0UL, budget.GetDetailedStatistics().CodecDecoder.LiveBytes);
    }

    [Fact]
    public void AnotherReaderCanReclaimCachedStorage()
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid()+".mcap");
        try {
            using (var writer = new McapWriter(path, new() { Compression = McapCompression.None })) {
                var channel = writer.RegisterChannel("t", "raw");
                writer.WriteMessage(new(channel, 1, 1, 0), new byte[70000]); writer.Complete();
            }
            var budget = new McapMemoryBudget(180000, 180000, 0);
            using var reader = McapIndexSnapshot.OpenMapped(path, new() { Budget = budget, MaxRandomAccessCacheBytes = 1000000 });
            using var index = new McapPreparedChunkIndex(reader.GetSummary()!.ChunkIndexes[0]);
            var entry = reader.ReadMessageIndexes(index)[0].Records[0];
            reader.SeekMessage(index, entry, static (in McapMessageHeader h, ReadOnlySpan<byte> data) => true);
            var used = budget.GetStatistics().CurrentBytes;
            Assert.True(used > 60000);
            // Copied input competes with this reader's recyclable index/descriptor pages.
            var bytes = new byte[checked((int)(budget.MaxBytes - used + 32768))];
            Assert.True((ulong)bytes.Length <= budget.MaxBlockBytes);
            using (var input = new McapBufferReader(bytes, McapBufferReadMode.Messages, false, new() { Budget = budget })) {
                Assert.InRange(budget.GetStatistics().CurrentBytes, 0UL, budget.MaxBytes);
            }
            reader.SeekMessage(index, entry, static (in McapMessageHeader h, ReadOnlySpan<byte> data) => true);
            Assert.Equal(2UL, reader.GetCacheStatistics().ChunkLoads);
        } finally { File.Delete(path); }
    }
    [Fact]
    public void SummaryPageFailureLeavesWriterTerminalAndReleasable()
    {
        // Control storage is independent; measure the real declaration/chunk/summary peak.
        using var prepared = McapPreparedOperation.Channel("t", "raw");
        var probeBudget = new McapMemoryBudget(4 * 1024 * 1024, 4096, 0);
        ulong beforeComplete;
        using (var probeStream = new MemoryStream())
        using (var probe = new McapWriter(probeStream, new() { Compression = McapCompression.None, ChunkSize = 512, EmitMessageIndexes = false, Memory = new() { Budget = probeBudget } }, true))
        {
            var channel = checked((ushort)probe.WritePrepared(prepared));
            probe.WriteMessage(new(channel, 1, 1, 0), new byte[16]);
            beforeComplete = probeBudget.GetStatistics().PeakBytes;
            probe.Complete();
        }
        var peak = probeBudget.GetStatistics().PeakBytes;
        Assert.True(peak > beforeComplete);
        var budget = new McapMemoryBudget(peak - 1, 4096, 0);
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = McapCompression.None, ChunkSize = 512, EmitMessageIndexes = false, Memory = new() { Budget = budget } }, true))
        {
            var channel = checked((ushort)writer.WritePrepared(prepared));
            writer.WriteMessage(new(channel, 1, 1, 0), new byte[16]);
            Assert.Throws<McapException>(() => writer.Complete());
            Assert.Throws<InvalidOperationException>(() => writer.Complete());
        }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void PreparedOperationUsesItsOwnDomainAcrossWritersAndRollsBackRefusal()
    {
        var budget = new McapMemoryBudget(1024 * 1024, 65536, 0);
        var payload = new byte[4096];
        Array.Fill(payload, (byte)42);
        ulong peak;
        using (var operation = McapPreparedOperation.Schema(budget, "s", "raw", payload))
        {
            var statistics = budget.GetDetailedStatistics();
            Assert.Equal(6UL + BudgetAssertions.ControlAllocations, statistics.AllocationCount);
            Assert.Equal(statistics.CurrentBytes - BudgetAssertions.ControlBytes, statistics.Declaration.LiveBytes);
            Assert.True(statistics.Flow.OtherCopyBytes >= 4096UL);
            peak = statistics.PeakBytes;
            payload[0] = 0;
            for (var i = 0; i < 2; i++)
            {
                using var stream = new MemoryStream();
                using var writer = new McapWriter(stream, new() { Compression = McapCompression.None }, true);
                Assert.NotEqual(0UL, writer.WritePrepared(operation));
                writer.Complete();
                Assert.Equal(statistics.CurrentBytes, budget.GetStatistics().CurrentBytes);
                writer.Dispose();
                stream.Position = 0;
                using var reader = McapReader.OpenRecords(stream, leaveOpen: true);
                Assert.Equal((byte)42, reader.ReadSchemas().First().Data[0]);
            }
            CheckTotals(budget);
        }
        BudgetAssertions.Idle(budget);
        var limited = new McapMemoryBudget(peak - 1, peak - 1, 0);
        Assert.Throws<McapException>(() => McapPreparedOperation.Schema(limited, "s", "raw", payload));
        BudgetAssertions.Idle(limited);
        var blockLimited = new McapMemoryBudget(1024 * 1024, 4095, 0);
        Assert.Throws<McapException>(() => McapPreparedOperation.Schema(blockLimited, "s", "raw", payload));
        BudgetAssertions.Idle(blockLimited);
        // Existing positional nullable-ID calls remain unambiguous.
        using var compatible = McapPreparedOperation.Schema("s", "raw", [], null);
    }

    [Fact]
    public void PreparedMetadataStreamsPagedFieldsAndPreservesSnapshot()
    {
        var budget = new McapMemoryBudget(2 * 1024 * 1024, 1024 * 1024, 0);
        var values = Enumerable.Range(0, 1000).Reverse().ToDictionary(i => $"key{i:D4}", i => $"值/{i}/🚀");
        using (var operation = McapPreparedOperation.Metadata("元数据", values, budget))
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
            CheckTotals(budget);
        }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void WriterConfigurationIsChargedBeforeOutputStarts()
    {
        ulong retained;
        var probeBudget = new McapMemoryBudget(maxRetainedBytes: 0);
        using (var probeStream = new MemoryStream())
        using (var probe = new McapWriter(probeStream, new() { UseChunks = false, Memory = new() { Budget = probeBudget } }, true))
            retained = probeBudget.GetStatistics().CurrentBytes;
        var budget = new McapMemoryBudget(retained + 4096, 4096, 0);
        using var stream = new MemoryStream();
        Assert.Throws<McapException>(() => new McapWriter(stream,
            new() { UseChunks = false, Memory = new() { Budget = budget } }, true));
        Assert.Equal(0, stream.Length);
        Assert.InRange(budget.GetStatistics().PeakBytes, 1UL, budget.MaxBytes);
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void RegularControlJsonUsesWriterBudgetAndRefusalIsTerminal()
    {
        ulong initializationPeak;
        var probeBudget = new McapMemoryBudget(maxRetainedBytes: 0);
        using (var probeStream = new MemoryStream())
        using (var probe = new McapWriter(probeStream, new() { UseChunks = false, Memory = new() { Budget = probeBudget } }, true))
            initializationPeak = probeBudget.GetStatistics().PeakBytes;
        var budget = new McapMemoryBudget(initializationPeak + 4096, 4096, 0);
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { UseChunks = false, Memory = new() { Budget = budget } }, true))
        {
            var before = budget.GetStatistics().CurrentBytes;
            var position = stream.Position;
            var error = Assert.Throws<McapException>(() => writer.RegisterSchema(new string('s', 8192), "raw", []));
            Assert.False(error.CanContinueWriting);
            Assert.Equal("StorageBlock", error.Details.GetProperty("resource").GetString());
            Assert.Equal(before, budget.GetStatistics().CurrentBytes);
            Assert.Equal(position, stream.Position);
            Assert.Throws<InvalidOperationException>(() => writer.Flush());
        }
        BudgetAssertions.Idle(budget);
    }

    [Fact]
    public void RegularMetadataControlChargesAndReleasesTransientJsonStorage()
    {
        var budget = new McapMemoryBudget(1024 * 1024, 65536, 0);
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { UseChunks = false, EmitMetadataIndexes = false, Memory = new() { Budget = budget } }, true))
        {
            var before = budget.GetDetailedStatistics();
            writer.WriteMetadata("meta", new Dictionary<string, string> { ["key"] = "值" });
            var after = budget.GetDetailedStatistics();
            Assert.Equal(before.CurrentBytes, after.CurrentBytes);
            Assert.True(after.AllocationCount > before.AllocationCount);
            Assert.True(after.Flow.OtherCopyBytes > before.Flow.OtherCopyBytes);
            CheckTotals(budget);
        }
        BudgetAssertions.Idle(budget);
    }

    static void CheckTotals(McapMemoryBudget budget)
    {
        var d = budget.GetDetailedStatistics();
        var categories = new[] { d.Input, d.Decompressed, d.Writer, d.CodecEncoder, d.CodecDecoder, d.Index, d.Descriptor, d.Declaration, d.Scratch };
        Assert.Equal(budget.GetStatistics().CurrentBytes, categories.Aggregate(0UL, (n, c) => n + c.CurrentBytes));
        foreach (var category in categories) Assert.Equal(category.CurrentBytes, category.LiveBytes + category.ReservedBytes);
    }
}
