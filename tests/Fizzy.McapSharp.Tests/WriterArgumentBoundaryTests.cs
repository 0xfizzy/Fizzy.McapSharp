using Xunit;

namespace Fizzy.McapSharp.Tests;

public class WriterArgumentBoundaryTests
{
    [Fact]
    public void PreparedResultsDistinguishChannelZeroFromOperationsWithoutIds()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, leaveOpen: true);
        using var schema = McapPreparedOperation.Schema("schema", "raw", [], id: 7);
        using var channel = McapPreparedOperation.Channel("topic", "raw", schemaId: 7, id: 0);
        using var metadata = McapPreparedOperation.Metadata("metadata", new Dictionary<string, string>());
        using var attachment = McapPreparedOperation.Attachment("attachment", "raw", 0, 0);
        using var start = McapPreparedOperation.StartAttachment("segmented", "raw", 0, 0, 0);
        Assert.Equal((ushort?)7, writer.WritePrepared(schema));
        Assert.Equal((ushort?)0, writer.WritePrepared(channel));
        Assert.Null(writer.WritePrepared(metadata));
        Assert.Null(writer.WritePrepared(attachment));
        Assert.Null(writer.WritePrepared(start));
        writer.FinishAttachment();
        writer.WriteMessage(new McapMessageHeader(0, 0, 1, 1), [42]);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
        Assert.Equal(2u, writer.GetSummary().Statistics!.AttachmentCount);
    }

    [Fact]
    public void RequiredNullArgumentsDoNotFailWriter()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, new() { SafeRejections = McapWriterSafeRejections.None }, leaveOpen: true);
        Action[] invalid =
        [
            () => writer.RegisterSchema(null!, "raw", []),
            () => writer.RegisterSchema(1, "schema", null!, []),
            () => writer.RegisterChannel(null!, "raw"),
            () => writer.RegisterChannel(1, "topic", null!),
            () => writer.WriteMetadata(null!, new Dictionary<string, string>()),
            () => writer.WriteMetadata("name", null!),
            () => writer.WriteAttachment(null!, "raw", 0, 0, []),
            () => writer.StartAttachment("name", null!, 0, 0, 0),
            () => writer.WriteMessage(new McapMessage(new McapChannel(1, "topic", "raw", null, new Dictionary<string, string>()), 0, 0, 0, null!)),
        ];
        foreach (var call in invalid) Assert.Throws<ArgumentNullException>(call);
        Assert.Throws<ArgumentException>(() => writer.RegisterChannel("topic", "raw", metadata: new Dictionary<string, string> { ["key"] = null! }));
        var channel = writer.RegisterChannel("valid", "raw");
        writer.WriteMessage(new McapMessageHeader(channel, 0, 1, 1), [42]);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
    }

    [Fact]
    public void NonAttachmentPreparedPayloadIsRejectedBeforeMutation()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, new() { SafeRejections = McapWriterSafeRejections.None }, leaveOpen: true);
        using var schema = McapPreparedOperation.Schema("schema", "raw", [1]);
        using var channel = McapPreparedOperation.Channel("topic", "raw");
        using var metadata = McapPreparedOperation.Metadata("metadata", new Dictionary<string, string>());
        using var start = McapPreparedOperation.StartAttachment("attachment", "raw", 0, 0, 0);
        foreach (var operation in new[] { schema, channel, metadata, start })
            Assert.Throws<ArgumentException>(() => writer.WritePrepared(operation, new byte[] { 9 }));
        var id = writer.WritePrepared(channel)!.Value;
        writer.WriteMessage(new McapMessageHeader(id, 0, 1, 1), [42]);
        using var attachment = McapPreparedOperation.Attachment("attachment", "raw", 0, 0);
        writer.WritePrepared(attachment, [7]);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
        Assert.Equal(1u, writer.GetSummary().Statistics!.AttachmentCount);
    }

    [Fact]
    public void PreparedFactoriesValidateRequiredArguments()
    {
        Assert.Throws<ArgumentNullException>(() => McapPreparedOperation.Schema(null!, "raw", []));
        Assert.Throws<ArgumentNullException>(() => McapPreparedOperation.Channel("topic", null!));
        Assert.Throws<ArgumentNullException>(() => McapPreparedOperation.Metadata("metadata", null!));
        Assert.Throws<ArgumentNullException>(() => McapPreparedOperation.Attachment("name", null!, 0, 0));
        Assert.Throws<ArgumentNullException>(() => McapPreparedOperation.StartAttachment(null!, "raw", 0, 0, 0));
    }
}
