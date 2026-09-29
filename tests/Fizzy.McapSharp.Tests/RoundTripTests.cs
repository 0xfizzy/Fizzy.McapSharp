using Xunit;

namespace Fizzy.McapSharp.Tests;
public sealed class RoundTripTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "mcapsharp-" + Guid.NewGuid());
    public RoundTripTests() => Directory.CreateDirectory(root);
    private string PathFor(string file) => Path.Combine(root, file);
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void RoundTripPreservesRecordsAndFilters(McapCompression compression)
    {
        var path = PathFor("recording.mcap");
        using (var writer = new McapWriter(path, new() { Compression = compression, ChunkSize = 64 }))
        {
            var schema = writer.RegisterSchema("type", "jsonschema", "{}"u8);
            var channel = writer.RegisterChannel("/camera", "json", schema, new Dictionary<string, string> { { "device", "测试" } });
            for (uint i = 0; i < 30; i++)
                writer.WriteMessage(new McapMessageHeader(channel, i, i * 10, i * 10 + 1), System.Text.Encoding.UTF8.GetBytes($"{{\"n\":{i}}}"));
            var other = writer.RegisterChannel("/motion", "binary");
            writer.WriteMessage(new McapMessageHeader(other, 0, 50, 51), [1, 2, 3]);
            writer.WriteMetadata("session", new Dictionary<string, string> { { "clock", "monotonic" } });
            writer.WriteAttachment("calibration", "application/octet-stream", 50, 40, [9, 8, 7]);
            writer.Complete();
            writer.Complete();
            Assert.Throws<InvalidOperationException>(() => writer.Flush());
        }

        var reader = new McapReader(path);
        Assert.True(reader.Validate() > 0);
        var messages = reader.ReadMessages(new() { Topic = "/camera", StartTime = 100, EndTime = 150 }).ToArray();
        Assert.Equal(5, messages.Length);
        Assert.Equal(10u, messages[0].Sequence);
        Assert.Equal("测试", messages[0].Channel.Metadata["device"]);
        Assert.Equal("{}"u8.ToArray(), messages[0].Channel.Schema!.Data);
        Assert.Equal("monotonic", reader.ReadMetadata().Single().Values["clock"]);
        Assert.Equal(new byte[] { 9, 8, 7 }, reader.ReadAttachments().Single().Data);
        Assert.Equal(31, reader.ReadMessages().Count());
    }

    [Fact]
    public void UnindexedFilesUseSequentialQuery()
    {
        var path = PathFor("linear.mcap");
        using (var w = new McapWriter(path, new() { EmitStatistics = false, EmitMessageIndexes = false, EmitChunkIndexes = false, EmitAttachmentIndexes = false, EmitMetadataIndexes = false, RepeatChannels = false, RepeatSchemas = false, UseChunks = false }))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 0, 3, 3), [42]);
            w.Complete();
        }

        var r = new McapReader(path);
        r.Validate();
        Assert.Equal(42, r.ReadMessages(new() { Topic = "t", StartTime = 3, EndTime = 4 }).Single().Data[0]);
    }

    [Fact]
    public void DisposeWithoutCompleteLeavesIncompleteFile()
    {
        var path = PathFor("incomplete.mcap");
        using (var w = new McapWriter(path))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), [42]);
        }

        Assert.Throws<McapException>(() => new McapReader(path).Validate());
    }

    [Fact]
    public void CorruptionAndTruncationAreRejected()
    {
        var path = PathFor("valid.mcap");
        using (var w = new McapWriter(path, new() { UseChunks = false }))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), "payload-marker"u8);
            w.Complete();
        }

        var bytes = File.ReadAllBytes(path);
        var index = bytes.AsSpan().IndexOf("payload-marker"u8);
        Assert.True(index > 0);
        bytes[index] ^= 1;
        var corrupt = PathFor("corrupt.mcap");
        File.WriteAllBytes(corrupt, bytes);
        Assert.Throws<McapException>(() => new McapReader(corrupt).Validate());
        File.WriteAllBytes(corrupt, bytes[..^5]);
        Assert.Throws<McapException>(() => new McapReader(corrupt).ReadMessages().ToArray());
    }

    [Fact]
    public void FailedWriterIsTerminalAndNeverOverwrites()
    {
        var path = PathFor("failure.mcap");
        using (var w = new McapWriter(path, new() { RecoverableErrors = McapRecoverableWriterErrors.None }))
        {
            Assert.Throws<McapException>(() => w.WriteMessage(new McapMessageHeader(99, 1, 1, 1), [1]));
            Assert.Throws<InvalidOperationException>(() => w.Complete());
        }

        Assert.Throws<McapException>(() => new McapWriter(path));
    }

    [Fact]
    public void EnumerationDisposalReleasesNativeFile()
    {
        var path = PathFor("release.mcap");
        using (var w = new McapWriter(path))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), [1]);
            w.Complete();
        }

        using (var e = new McapReader(path).ReadMessages().GetEnumerator())
        {
            Assert.True(e.MoveNext());
            if (OperatingSystem.IsWindows())
                Assert.Throws<IOException>(() => File.OpenWrite(path));
        }

        File.Delete(path);
    }

    [Fact]
    public void RecoveryReportsIncompleteInsteadOfSuccess()
    {
        var path = PathFor("recover.mcap");
        using (var w = new McapWriter(path, new() { UseChunks = false }))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), [42]);
            w.Flush();
        }

        var messages = new List<McapMessage>();
        var result = new McapReader(path).RecoverMessages(messages.Add);
        Assert.False(result.IsComplete);
        Assert.NotNull(result.Error);
        Assert.Single(messages);
        Assert.Equal(1ul, result.RecoveredMessageCount);
    }

    [Fact]
    public void SchemasAndChannelsExistWithoutMessages()
    {
        var path = PathFor("empty.mcap");
        using (var w = new McapWriter(path))
        {
            var s = w.RegisterSchema("schema", "jsonschema", "{}"u8);
            w.RegisterChannel("empty", "json", s);
            w.Complete();
        }

        var reader = new McapReader(path);
        reader.Validate();
        Assert.Single(reader.ReadSchemas());
        Assert.Equal("schema", reader.ReadChannels().Single().Schema!.Name);
        Assert.Empty(reader.ReadMessages());
    }

    [Theory]
    [InlineData("attachment-marker")]
    [InlineData("payload-marker")]
    [InlineData("schema-marker")]
    public void FullValidationDetectsCrcCorruptionAcrossSections(string marker)
    {
        var path = PathFor("crc.mcap");
        using (var w = new McapWriter(path, new() { Compression = McapCompression.None }))
        {
            var s = w.RegisterSchema("schema-marker", "raw", []);
            var c = w.RegisterChannel("t", "raw", s);
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), "payload-marker"u8);
            w.WriteAttachment("file", "raw", 1, 1, "attachment-marker"u8);
            w.Complete();
        }

        var bytes = File.ReadAllBytes(path);
        var target = System.Text.Encoding.UTF8.GetBytes(marker);
        var index = marker == "schema-marker" ? bytes.AsSpan().LastIndexOf(target) : bytes.AsSpan().IndexOf(target);
        Assert.True(index > 0);
        bytes[index] ^= 1;
        File.WriteAllBytes(path, bytes);
        Assert.Throws<McapException>(() => new McapReader(path).Validate());
    }

    [Theory]
    [InlineData(true, false, System.Runtime.InteropServices.Architecture.X64, true)]
    [InlineData(true, false, System.Runtime.InteropServices.Architecture.Arm64, false)]
    [InlineData(false, true, System.Runtime.InteropServices.Architecture.X64, true)]
    [InlineData(false, true, System.Runtime.InteropServices.Architecture.Arm64, true)]
    [InlineData(false, true, System.Runtime.InteropServices.Architecture.X86, false)]
    [InlineData(false, true, System.Runtime.InteropServices.Architecture.Arm, false)]
    [InlineData(false, false, System.Runtime.InteropServices.Architecture.X64, false)]
    public void PlatformMatrix(bool windows, bool linux, System.Runtime.InteropServices.Architecture architecture, bool supported) => Assert.Equal(supported, Native.IsSupportedPlatform(windows, linux, architecture));
    [Fact]
    public void UnicodePathAndOwnedDataSurviveEnumeratorDisposal()
    {
        var path = PathFor("机器人 sample.mcap");
        using (var writer = new McapWriter(path))
        {
            var channel = writer.RegisterChannel("test", "raw");
            writer.WriteMessage(new McapMessageHeader(channel, 0, 1, 1), [1, 2, 3]);
            writer.Complete();
        }

        McapMessage message;
        using (var enumerator = new McapReader(path).ReadMessages().GetEnumerator())
        {
            Assert.True(enumerator.MoveNext());
            message = enumerator.Current;
        }

        File.Delete(path);
        Assert.Equal(new byte[] { 1, 2, 3 }, message.Data);
    }

    public void Dispose() => Directory.Delete(root, true);
}
