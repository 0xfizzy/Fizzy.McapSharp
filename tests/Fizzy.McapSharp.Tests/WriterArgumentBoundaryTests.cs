using Xunit;

namespace Fizzy.McapSharp.Tests;

public class WriterArgumentBoundaryTests
{
    [Fact]
    public void PreparedResultsDistinguishChannelZeroFromOperationsWithoutIds()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, leaveOpen: true);
        using var schema = new McapPreparedSchema("schema", "raw", [], id: 7);
        using var channel = new McapPreparedChannelRegistration("topic", "raw", schemaId: 7, id: 0);
        using var metadata = new McapPreparedMetadata("metadata", new Dictionary<string, string>());
        using var attachment = new McapPreparedAttachment("attachment", "raw", 0, 0);
        using var start = new McapPreparedAttachmentStart("segmented", "raw", 0, 0, 0);
        Assert.Equal((ushort?)7, writer.RegisterSchema(schema));
        Assert.Equal((ushort?)0, writer.RegisterChannel(channel));
        writer.WriteMetadata(metadata);
        writer.WriteAttachment(attachment);
        writer.StartAttachment(start);
        writer.FinishAttachment();
        writer.WriteToKnownChannel(new McapMessageHeader(0, 0, 1, 1), [42]);
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
        writer.WriteToKnownChannel(new McapMessageHeader(channel, 0, 1, 1), [42]);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
    }

    [Fact]
    public void PreparedPayloadIsExposedOnlyForAttachmentWriting()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, new() { SafeRejections = McapWriterSafeRejections.None }, leaveOpen: true);
        using var schema = new McapPreparedSchema("schema", "raw", [1]);
        using var channel = new McapPreparedChannelRegistration("topic", "raw");
        using var metadata = new McapPreparedMetadata("metadata", new Dictionary<string, string>());
        using var start = new McapPreparedAttachmentStart("attachment", "raw", 0, 0, 0);
        Assert.DoesNotContain(typeof(McapWriter).GetMethods(), m => m.Name == "WritePrepared");
        foreach (var descriptor in new[] { typeof(McapPreparedSchema), typeof(McapPreparedChannelRegistration), typeof(McapPreparedMetadata), typeof(McapPreparedAttachmentStart) })
            Assert.All(typeof(McapWriter).GetMethods().Where(m => m.GetParameters().Any(p => p.ParameterType == descriptor)),
                m => Assert.Single(m.GetParameters()));
        var id = writer.RegisterChannel(channel);
        writer.WriteToKnownChannel(new McapMessageHeader(id, 0, 1, 1), [42]);
        using var attachment = new McapPreparedAttachment("attachment", "raw", 0, 0);
        writer.WriteAttachment(attachment, [7]);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
        Assert.Equal(1u, writer.GetSummary().Statistics!.AttachmentCount);
    }

    [Fact]
    public void PreparedFactoriesValidateRequiredArguments()
    {
        Assert.Throws<ArgumentNullException>(() => new McapPreparedSchema(null!, "raw", []));
        Assert.Throws<ArgumentNullException>(() => new McapPreparedChannelRegistration("topic", null!));
        Assert.Throws<ArgumentNullException>(() => new McapPreparedMetadata("metadata", null!));
        Assert.Throws<ArgumentNullException>(() => new McapPreparedAttachment("name", null!, 0, 0));
        Assert.Throws<ArgumentNullException>(() => new McapPreparedAttachmentStart(null!, "raw", 0, 0, 0));
    }
}
