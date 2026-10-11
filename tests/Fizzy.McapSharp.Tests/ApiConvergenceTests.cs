using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ApiConvergenceTests
{
    static byte[] Recording(McapCompression compression, bool chunks = true)
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { Compression = compression, CompressionThreads = 0, UseChunks = chunks }, true))
        {
            var channel = new McapChannel(7, "topic", "raw", null, new Dictionary<string, string>());
            var message = new McapMessage(channel, 13, 101, 211, [1, 2, 3]);
            var (_, sequence, logTime, publishTime, _) = message;
            Assert.Equal(13u, sequence); Assert.Equal(101ul, logTime); Assert.Equal(211ul, publishTime);
            writer.WriteMessage(message);
            using var prepared = new McapPreparedChannel(channel);
            writer.WriteMessage(prepared, new(7, 17, 103, 223), [4, 5, 6]);
            writer.WriteBatch(new McapMessageHeader[] { new(7, 19, 107, 227) }, new byte[] { 7, 8, 9 }, new McapPayloadRange[] { new(0, 3) });
            writer.Complete();
        }
        return stream.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void ValidationCapabilityMatchesExecutionAndSurvivesDisposal(McapCompression compression)
    {
        var bytes = Recording(compression);
        string path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        File.WriteAllBytes(path, bytes);
        try
        {
            var factory = new McapReaderFactory(path);
            foreach (bool strict in new[] { false, true })
            {
                var options = strict ? McapReaderOptions.Strict : null;
                var expected = strict ? McapValidationCapability.StrictFullScan : McapValidationCapability.FullScan;
                McapMessageReader[] readers = [factory.OpenMessages(new() { Topic = "topic" }, options),
                    McapReaderFactory.OpenMessages(new MemoryStream(bytes), options: options),
                    McapReaderFactory.OpenMessages(new CountingInput(bytes, false), options: options),
                    new McapMessageReader(bytes, options), McapMessageReader.OpenMapped(path, options)];
                foreach (var reader in readers)
                {
                    using (reader)
                    {
                        Assert.Equal(expected, reader.ValidationCapability);
                        if (strict) { Assert.True(reader.ValidateRemaining() > 0); Assert.True(reader.IsFullyValidated); }
                        else Assert.Equal(3, reader.ReadMessages().Count());
                    }
                    Assert.Equal(expected, reader.ValidationCapability);
                }
                using var records = factory.OpenRecords(options);
                Assert.Equal(expected, records.ValidationCapability);
                using var copiedRecords = new McapChunkFlattener(bytes, options);
                Assert.Equal(expected, copiedRecords.ValidationCapability);
                using var mappedRecords = McapChunkFlattener.OpenMapped(path, options);
                Assert.Equal(expected, mappedRecords.ValidationCapability);
            }
            using var indexed = factory.OpenIndexedMessages();
            Assert.Equal(McapValidationCapability.PartialInput, indexed.ValidationCapability);
            using var indexedQuery = factory.QueryMessages();
            Assert.Equal(McapValidationCapability.PartialInput, indexedQuery.ValidationCapability);
            using var sorted = McapReaderFactory.QueryMessages(new CountingInput(bytes, false), options: McapReaderOptions.Strict);
            Assert.Equal(McapValidationCapability.PartialInput, sorted.ValidationCapability);
            AssertRejectedWithoutConsumption(sorted);
            using var snapshot = McapIndexedReader.CreateCopied(bytes);
            using var chunk = snapshot.OpenChunkMessages(snapshot.GetSummary()!.ChunkIndexes[0]);
            Assert.Equal(McapValidationCapability.PartialInput, chunk.ValidationCapability);
            AssertRejectedWithoutConsumption(chunk);
            AssertRejectedWithoutConsumption(indexed);
        }
        finally { File.Delete(path); }
    }

    static void AssertRejectedWithoutConsumption(McapMessageReader reader)
    {
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var pending, out var required));
        Assert.Throws<InvalidOperationException>(() => reader.ValidateRemaining());
        Assert.Throws<InvalidOperationException>(() => reader.RecoverMessages(_ => throw new Exception("Must not deliver")));
        Assert.Equal(McapReadStatus.Success, reader.ReadNext(new byte[checked((int)required)], out var actual, out _));
        Assert.Equal(pending, actual);
    }

    [Fact]
    public void NonStrictRejectionDoesNotPerformIoAndFailureTakesPrecedence()
    {
        using var stream = new CountingInput(Recording(McapCompression.None), true);
        using var reader = McapReaderFactory.OpenMessages(stream, leaveOpen: true);
        Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var pending, out _));
        int reads = stream.Reads; long position = stream.Position;
        AssertRejectedWithoutConsumption(reader);
        Assert.Equal(reads, stream.Reads); Assert.Equal(position, stream.Position);
        var expected = new ApplicationException("visitor");
        Assert.Same(expected, Assert.Throws<ApplicationException>(() => reader.ReadNext((in McapMessageHeader _, ReadOnlySpan<byte> _) => throw expected)));
        Assert.Equal(McapValidationCapability.FullScan, reader.ValidationCapability);
        Assert.Throws<InvalidOperationException>(() => reader.ValidateRemaining());
        reader.Dispose();
        Assert.Throws<ObjectDisposedException>(() => reader.ValidateRemaining());
        Assert.Throws<ObjectDisposedException>(() => reader.RecoverMessages(_ => { }));
        Assert.Equal(McapValidationCapability.FullScan, reader.ValidationCapability);
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void StrictRecoveryContinuesCurrentCursorAndPreservesMessageFields(McapCompression compression)
    {
        using var reader = new McapMessageReader(Recording(compression), McapReaderOptions.Strict);
        using var first = reader.ReadBatchLease(1)!;
        Assert.Equal(new McapMessageHeader(7, 13, 101, 211), first.GetHeader(0));
        var messages = new List<McapMessage>();
        var recovered = reader.RecoverMessages(messages.Add);
        Assert.Equal(2ul, recovered.RecoveredMessageCount); Assert.True(recovered.IsFullyValidated);
        Assert.Equal(new uint[] { 17, 19 }, messages.Select(m => m.Sequence));
        Assert.Equal(new ulong[] { 103, 107 }, messages.Select(m => m.LogTime));
        Assert.Equal(new ulong[] { 223, 227 }, messages.Select(m => m.PublishTime));
        reader.Dispose();
        Assert.Equal(new byte[] { 1, 2, 3 }, first.GetPayload(0).ToArray());
    }

    static McapSansIoSummaryReader Summary(byte[] bytes)
    {
        var reader = new McapSansIoSummaryReader();
        using var source = new MemoryStream(bytes);
        Span<byte> input = stackalloc byte[7];
        while (reader.NextEvent(out var e) != McapReadStatus.EndOfStream)
        {
            if (e.Kind == McapSummaryEventKind.Seek)
                reader.NotifySeeked(checked((ulong)source.Seek(e.Seek.Origin == SeekOrigin.Begin ? checked((long)e.Seek.AbsolutePosition) : e.Seek.RelativeOffset, e.Seek.Origin)));
            else reader.SupplyInput(input[..source.Read(input[..(int)Math.Min((ulong)input.Length, e.Length)])]);
        }
        return reader;
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void PublicIndexedProtocolHasExactChunkRequestsAndMessageRetries(McapCompression compression)
    {
        var bytes = Recording(compression);
        using var summary = Summary(bytes);
        using var indexed = summary.CreateIndexed();
        var messages = new List<McapMessageHeader>();
        byte[] destination = [];
        while (true)
        {
            var status = indexed.NextEvent(destination, out var e);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall) { destination = new byte[checked((int)e.Length)]; continue; }
            if (e.Kind == McapIndexedEventKind.ReadChunk)
                indexed.InsertChunkData(e.Offset, bytes.AsSpan(checked((int)e.Offset), checked((int)e.Length)));
            else { messages.Add(e.Header); destination = []; }
        }
        Assert.Equal(new uint[] { 13, 17, 19 }, messages.Select(m => m.Sequence));
        using var invalid = summary.CreateIndexed();
        Assert.Equal(McapReadStatus.Success, invalid.NextEvent([], out var request));
        Assert.Equal(McapIndexedEventKind.ReadChunk, request.Kind);
        Assert.Throws<McapException>(() => invalid.InsertChunkData(request.Offset, []));
        Assert.Throws<InvalidOperationException>(() => invalid.NextEvent([], out _));
    }

    [Fact]
    public void SummaryRejectsWrongResponseKindAndLinearRejectsExcessInput()
    {
        using var summary = new McapSansIoSummaryReader();
        Assert.Equal(McapReadStatus.Success, summary.NextEvent(out var request));
        Assert.Equal(McapSummaryEventKind.Seek, request.Kind);
        Assert.Throws<McapException>(() => summary.SupplyInput([]));
        Assert.Throws<InvalidOperationException>(() => summary.NextEvent(out _));
        using var other = new McapSansIoSummaryReader();
        other.NextEvent(out _); other.NotifySeeked(100); other.NextEvent(out var read);
        Assert.Equal(McapSummaryEventKind.Read, read.Kind);
        Assert.Throws<McapException>(() => other.NotifySeeked(0));
        using var linear = new McapSansIoLinearReader();
        linear.NextEvent([], out var input);
        Assert.Throws<McapException>(() => linear.SupplyInput(new byte[checked((int)input.Length + 1)]));
        Assert.Throws<InvalidOperationException>(() => linear.NextEvent([], out _));
    }

    [Fact]
    public void ProtocolInputWithoutRequestAndPrematureEofAreTerminal()
    {
        using var early = new McapSansIoLinearReader();
        Assert.Throws<McapException>(() => early.SupplyInput([]));
        Assert.Throws<InvalidOperationException>(() => early.NextEvent([], out _));
        using var eof = new McapSansIoLinearReader();
        eof.NextEvent([], out _); eof.SupplyInput([]);
        Assert.Throws<McapException>(() => eof.NextEvent([], out _));
        using var summary = Summary(Recording(McapCompression.None));
        using var indexed = summary.CreateIndexed();
        Assert.Throws<McapException>(() => indexed.InsertChunkData(0, []));
        Assert.Throws<InvalidOperationException>(() => indexed.NextEvent([], out _));
        using var wrongOffset = summary.CreateIndexed();
        wrongOffset.NextEvent([], out var request);
        Assert.Throws<McapException>(() => wrongOffset.InsertChunkData(request.Offset + 1, new byte[checked((int)request.Length)]));
        Assert.Throws<InvalidOperationException>(() => wrongOffset.NextEvent([], out _));
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void RecordAccessorPreservesPrefixedSourceAndCopiedReaderOwnsInput(McapCompression compression)
    {
        var bytes = Recording(compression);
        using var source = new MemoryStream();
        source.Write(new byte[23]); source.Write(bytes); source.Position = 23;
        McapIndexedReader copied;
        McapChunkIndex index;
        using (var accessor = McapReaderFactory.OpenRecordAccessor(source, leaveOpen: true))
        {
            Assert.Equal(23, source.Position);
            var header = accessor.ReadRecordAt(8);
            Assert.Equal((byte)McapOpcode.Header, header.Opcode);
            Assert.Equal(23, source.Position);
            index = accessor.GetSummary()!.ChunkIndexes[0];
            copied = accessor.CreateCopiedIndexedReader();
            Assert.Equal(23, source.Position);
        }
        source.Dispose();
        using (copied)
        {
            using var cursor = copied.OpenChunkMessages(index);
            var entry = copied.ReadMessageIndexes(index).Single().Records[0];
            var message = copied.SeekMessage(index, entry);
            Assert.Equal(13u, message.Sequence); Assert.Equal(101ul, message.LogTime); Assert.Equal(211ul, message.PublishTime);
            copied.Dispose();
            Assert.Equal(3, cursor.ReadMessages().Count());
        }
        using var owned = McapIndexedReader.CreateCopied(bytes);
        Array.Clear(bytes);
        Assert.Equal(3, owned.ReadChunkMessages(index).Count());
    }

    [Fact]
    public void FileOrderedQueryReportsActualSequentialScope()
    {
        var bytes = Recording(McapCompression.None, chunks: false);
        using var ordinary = McapReaderFactory.QueryMessages(new MemoryStream(bytes), new() { Order = McapReadOrder.File });
        Assert.Equal(McapValidationCapability.FullScan, ordinary.ValidationCapability);
        using var strict = McapReaderFactory.QueryMessages(new MemoryStream(bytes), new() { Order = McapReadOrder.File, Topic = "topic" }, options: McapReaderOptions.Strict);
        Assert.Equal(McapValidationCapability.StrictFullScan, strict.ValidationCapability);
        Assert.True(strict.ValidateRemaining() > 0);
    }

    [Fact]
    public void SeekPositionsPreserveUnsignedAbsoluteAndSignedRelativeRanges()
    {
        var absolute = new McapSeekRequest(SeekOrigin.Begin, ulong.MaxValue);
        Assert.Equal(ulong.MaxValue, absolute.AbsolutePosition);
        Assert.Throws<InvalidOperationException>(() => absolute.RelativeOffset);
        foreach (long offset in new[] { long.MinValue, -53L, 0L, 71L, long.MaxValue })
            foreach (var origin in new[] { SeekOrigin.Current, SeekOrigin.End })
            {
                var relative = new McapSeekRequest(origin, unchecked((ulong)offset));
                Assert.Equal(offset, relative.RelativeOffset);
                Assert.Throws<InvalidOperationException>(() => relative.AbsolutePosition);
            }
    }

    [Fact]
    public void RemovedApisCannotBeSelectedAccidentally()
    {
        var names = typeof(McapMessageReader).Assembly.GetExportedTypes().Select(t => t.Name).ToArray();
        foreach (var name in new[] { "McapIndexSnapshot", "McapIndexSnapshotOptions", "McapRandomAccessReader" }) Assert.DoesNotContain(name, names);
        Assert.Empty(typeof(McapIndexedReader).GetConstructors());
        Assert.Null(typeof(McapSansIoIndexedReader).GetMethod("SupplyInput"));
        Assert.Null(typeof(McapSansIoIndexedReader).GetMethod("NotifySeeked"));
        Assert.Equal(new[] { "End", "Message", "ReadChunk" }, Enum.GetNames<McapIndexedEventKind>());
        Assert.Single(typeof(McapSansIoSummaryReader).GetMethod("NextEvent")!.GetParameters());
    }

    sealed class CountingInput(byte[] bytes, bool seekable) : Stream
    {
        readonly MemoryStream inner = new(bytes);
        public int Reads { get; private set; }
        public override int Read(Span<byte> buffer) { Reads++; return inner.Read(buffer); }
        public override int Read(byte[] buffer, int offset, int count) => Read(buffer.AsSpan(offset, count));
        public override bool CanRead => true;
        public override bool CanSeek => seekable;
        public override bool CanWrite => false;
        public override long Length => seekable ? inner.Length : throw new NotSupportedException();
        public override long Position { get => inner.Position; set { if (!seekable) throw new NotSupportedException(); inner.Position = value; } }
        public override long Seek(long offset, SeekOrigin origin) => seekable ? inner.Seek(offset, origin) : throw new NotSupportedException();
        public override void Flush() { }
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        protected override void Dispose(bool disposing) { if (disposing) inner.Dispose(); base.Dispose(disposing); }
    }
}
