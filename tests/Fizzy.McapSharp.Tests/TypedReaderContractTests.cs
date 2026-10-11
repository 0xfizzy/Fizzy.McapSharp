using System.Buffers.Binary;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class TypedReaderContractTests
{
    [Fact]
    public void ExpandedClassificationsRejectEncodedChunkDelivery()
    {
        using var stream = new MemoryStream(Recording());
        var options = new McapReaderOptions { EmitChunks = true };
        Assert.Throws<ArgumentException>(() => McapReaderFactory.ReadSchemas(stream, true, options).ToArray());
        Assert.Throws<ArgumentException>(() => McapReaderFactory.ReadChannels(stream, true, options).ToArray());
        Assert.Throws<ArgumentException>(() => McapReaderFactory.ReadMetadata(stream, true, options).ToArray());
        Assert.Throws<ArgumentException>(() => McapReaderFactory.ReadAttachments(stream, true, options).ToArray());
        Assert.Equal(0, stream.Position);
    }

    [Theory]
    [InlineData(0)] [InlineData(1)] [InlineData(2)]
    public void CallbackFailureIsTerminalAcrossInputsAndRetainedResultsSurvive(int input)
    {
        var data = Recording();
        string path = Save(data);
        try
        {
            using var reader = input switch
            {
                0 => McapReaderFactory.OpenMessages(new MemoryStream(data)),
                1 => new McapMessageReader(data),
                _ => McapMessageReader.OpenMapped(path)
            };
            using var first = reader.ReadBatchLease(1);
            var description = reader.GetChannelDescription(11);
            var expected = new ApplicationException("callback failed");
            Assert.Same(expected, Assert.Throws<ApplicationException>(() => reader.ReadNext(
                (in McapMessageHeader _, ReadOnlySpan<byte> _) => throw expected)));
            Assert.Throws<InvalidOperationException>(() => reader.ReadNext(new byte[3], out _, out _));
            Assert.Throws<InvalidOperationException>(() => reader.GetChannelDescription(11));
            reader.Dispose();
            Assert.Throws<ObjectDisposedException>(() => reader.GetChannelDescription(11));
            Assert.Equal("topic", description.Topic);
            Assert.Equal(3, first!.GetPayload(0).Length);
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void PublicSansIoLinearProtocolPreservesRecordNegotiation()
    {
        var data = Recording();
        using var reader = new McapSansIoLinearReader();
        int position = 0, messages = 0;
        byte[] buffer = [];
        while (true)
        {
            var status = reader.NextEvent(buffer, out var item);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)item.Length)];
                continue;
            }
            if (item.Kind == McapLinearEventKind.Read)
            {
                int count = (int)Math.Min(item.Length, (ulong)(data.Length - position));
                reader.SupplyInput(data.AsSpan(position, count));
                position += count;
            }
            else if (item.Kind == McapLinearEventKind.Record && item.Opcode == 5) messages++;
        }
        Assert.Equal(3, messages);
    }

    [Fact]
    public void OwnedMetadataAndAttachmentWritesConsumeCurrentInputs()
    {
        using var output = new MemoryStream();
        var payload = new byte[] { 1, 2 };
        var values = new Dictionary<string, string> { ["key"] = "before" };
        using (var writer = new McapWriter(output, leaveOpen: true))
        {
            writer.WriteMetadata(new McapMetadata("meta", values));
            writer.WriteAttachment(new McapAttachment("attachment", "raw", 10, 20, payload));
            payload[0] = 99;
            values["key"] = "after";
            writer.Complete();
        }
        var data = output.ToArray();
        Assert.Equal("before", McapReaderFactory.ReadMetadata(new MemoryStream(data)).Single().Values["key"]);
        Assert.Equal(new byte[] { 1, 2 }, McapReaderFactory.ReadAttachments(new MemoryStream(data)).Single().Data);
    }

    static byte[] Recording(bool chunks = true, int payloadLength = 3, bool repeatSchemas = true)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new()
        {
            UseChunks = chunks, RepeatSchemas = repeatSchemas,
            CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false
        }, true))
        {
            writer.RegisterSchema(7, "schema", "raw", [8, 9]);
            writer.RegisterChannel(11, "topic", "raw", 7);
            foreach (var (sequence, time) in new[] { (1u, 30ul), (2u, 10ul), (3u, 20ul) })
                writer.WriteToKnownChannel(new(11, sequence, time, time), new byte[payloadLength]);
            writer.Complete();
        }
        return output.ToArray();
    }

    static string Save(byte[] bytes)
    {
        string path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        File.WriteAllBytes(path, bytes);
        return path;
    }

    [Fact]
    public void PublicCapabilitiesDoNotExposeRuntimeModeSwitches()
    {
        var assembly = typeof(McapMessageReader).Assembly;
        foreach (var name in new[] { "McapReadCursor", "McapReadSession", "McapCursorMode", "McapRecordMode", "McapAsyncReader", "McapSansIoReader", "McapPreparedOperation" })
            Assert.DoesNotContain(assembly.GetExportedTypes(), type => type.Name == name);
        Assert.Null(typeof(McapRawMessageReader).GetMethod("ReadMessages"));
        Assert.Null(typeof(McapLinearReader).GetMethod("ReadNext"));
        Assert.Null(typeof(McapSansIoLinearReader).GetMethod("NotifySeeked"));
        Assert.Null(typeof(McapSansIoSummaryReader).GetMethod("InsertChunkData"));
        Assert.Null(typeof(McapMessageFilter).GetProperty("Order"));
        Assert.Null(typeof(McapIndexedQuery).GetProperty("AllowBufferedSort"));
    }

    [Fact]
    public void DefaultsAndFiltersDoNotSilentlyChangeExecution()
    {
        string path = Save(Recording());
        try
        {
            var factory = new McapReaderFactory(path);
            Assert.Equal(new uint[] { 1, 2, 3 }, factory.ReadMessages().Select(m => m.Sequence));
            Assert.Equal(new uint[] { 1, 2, 3 }, factory.ReadMessages(new()).Select(m => m.Sequence));
            Assert.Equal(new uint[] { 1, 2, 3 }, factory.ReadMessages(new() { Topic = "topic" }).Select(m => m.Sequence));
            using var indexedDefault = factory.OpenIndexedMessages();
            using var indexedEmpty = factory.OpenIndexedMessages(new());
            using var indexedFile = factory.OpenIndexedMessages(new() { Order = McapReadOrder.File });
            Assert.Equal(new uint[] { 2, 3, 1 }, indexedDefault.ReadMessages().Select(m => m.Sequence));
            Assert.Equal(new uint[] { 2, 3, 1 }, indexedEmpty.ReadMessages().Select(m => m.Sequence));
            Assert.Equal(new uint[] { 1, 2, 3 }, indexedFile.ReadMessages().Select(m => m.Sequence));
            Assert.Equal(new uint[] { 2, 3, 1 }, factory.ReadQueryMessages().Select(m => m.Sequence));
            Assert.Equal(new uint[] { 2, 3, 1 }, factory.ReadQueryMessages(new()).Select(m => m.Sequence));
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(0, 0)] [InlineData(1, 0)] [InlineData(2, 0)] [InlineData(3, 0)]
    [InlineData(0, 3)] [InlineData(1, 3)] [InlineData(2, 3)] [InlineData(3, 3)]
    public void WireBodyNegotiationSharesPendingMessageWithOtherDelivery(int source, int payloadLength)
    {
        var data = Recording(payloadLength: payloadLength);
        string path = Save(data);
        try
        {
            using var snapshot = McapIndexedReader.CreateCopied(data);
            using var reader = source switch
            {
                0 => McapReaderFactory.OpenMessages(new MemoryStream(data)),
                1 => new McapMessageReader(data),
                2 => McapMessageReader.OpenMapped(path),
                _ => snapshot.OpenChunkMessages(Assert.Single(snapshot.GetSummary()!.ChunkIndexes))
            };
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextMessageRecord([], out var required));
            Assert.Equal((ulong)(22 + payloadLength), required);
            Assert.Equal(McapReadStatus.Success, reader.ReadNext(new byte[payloadLength], out var header, out var copied));
            Assert.Equal(1u, header.Sequence);
            Assert.Equal((ulong)payloadLength, copied);
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNextMessageRecord(new byte[21], out required));
            var body = new byte[checked((int)required)];
            Assert.Equal(McapReadStatus.Success, reader.ReadNextMessageRecord(body, out copied));
            Assert.Equal(required, copied);
            var parsed = Assert.IsType<McapMessageRecord>(McapRecords.Parse(5, body));
            Assert.Equal(2u, parsed.Header.Sequence);
            using var lease = reader.ReadBatchLease(1);
            Assert.NotNull(lease);
            Assert.Equal(payloadLength, lease.GetPayload(0).Length);
            Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNextMessageRecord([], out _));
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(false)] [InlineData(true)]
    public async Task ExpandedValidationIsIndependentOfInputAndCrcPreset(bool strict)
    {
        var data = Recording(chunks: false);
        for (int offset = 8; offset < data.Length - 8;)
        {
            int length = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset + 1)));
            if (data[offset] == 5)
            {
                BinaryPrimitives.WriteUInt16LittleEndian(data.AsSpan(offset + 9), 123);
                break;
            }
            offset += 9 + length;
        }
        var options = strict ? McapReaderOptions.Strict : new McapReaderOptions();
        string path = Save(data);
        try
        {
            foreach (int input in new[] { 0, 1, 2 })
            {
                using var reader = input switch
                {
                    0 => McapReaderFactory.OpenRecords(new MemoryStream(data), options: options),
                    1 => new McapChunkFlattener(data, options),
                    _ => McapChunkFlattener.OpenMapped(path, options)
                };
                byte[] buffer = new byte[4096];
                var error = Assert.Throws<McapException>(() =>
                {
                    while (reader.ReadNextRecord(buffer, out _, out _) != McapReadStatus.EndOfStream) { }
                });
                Assert.Equal(McapErrorKind.UnknownChannel, error.Kind);
                Assert.Throws<InvalidOperationException>(() => reader.GetChannelDescription(11));
                Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecord(buffer, out _, out _));
            }
            using var asyncReader = new McapAsyncRecordReader(new MemoryStream(data), options, inputBufferSize: 3);
            var asyncError = await Assert.ThrowsAsync<McapException>(async () =>
            {
                byte[] buffer = new byte[4096];
                while ((await asyncReader.ReadNextRecordAsync(buffer)).Status != McapReadStatus.EndOfStream) { }
            });
            Assert.Equal(McapErrorKind.UnknownChannel, asyncError.Kind);
            Assert.Throws<InvalidOperationException>(() => asyncReader.GetChannelDescription(11));
            using var raw = new McapRawMessageReader(data, options);
            Assert.Equal(McapReadStatus.Success, raw.ReadNext(new byte[3], out var header, out _));
            Assert.Equal((ushort)123, header.ChannelId);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(0)] [InlineData(1)] [InlineData(2)]
    public void ExpandedDeclarationsAndFullValidationHaveTheSameMeaning(int input)
    {
        var data = Recording();
        string path = Save(data);
        try
        {
            using var reader = input switch
            {
                0 => McapReaderFactory.OpenRecords(new MemoryStream(data), options: McapReaderOptions.Strict),
                1 => new McapChunkFlattener(data, McapReaderOptions.Strict),
                _ => McapChunkFlattener.OpenMapped(path, McapReaderOptions.Strict)
            };
            Assert.Throws<McapException>(() => reader.GetSchemaDescription(7));
            Assert.False(reader.IsFullyValidated);
            Assert.True(reader.ValidateRemaining() > 0);
            Assert.True(reader.IsFullyValidated);
            Assert.True(reader.IsScanComplete);
            var schema = reader.GetSchemaDescription(7);
            Assert.Same(schema, reader.GetChannelDescription(11).Schema);
            reader.GetSchema(7).Data[0] = 99;
            Assert.Equal(new byte[] { 8, 9 }, schema.Data.ToArray());
            reader.Dispose();
            Assert.Equal("schema", schema.Name);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(false)] [InlineData(true)]
    public void SummaryContainsOwnedDeclarationsWithoutInventingRepeatedSchemas(bool repeatSchemas)
    {
        var data = Recording(repeatSchemas: repeatSchemas);
        var first = McapReaderFactory.GetSummary(new MemoryStream(data))!;
        Assert.Equal(repeatSchemas ? 1 : 0, first.Schemas.Count);
        Assert.Equal(new byte[] { 8, 9 }, first.Channels[11].Schema!.Data);
        if (repeatSchemas) Assert.Same(first.Schemas[7], first.Channels[11].Schema);
        var second = McapReaderFactory.GetSummary(new MemoryStream(data))!;
        first.Channels[11].Schema!.Data[0] = 99;
        Assert.Equal(new byte[] { 8, 9 }, second.Channels[11].Schema!.Data);
    }
}
