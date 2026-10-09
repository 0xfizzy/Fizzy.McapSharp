using Xunit;

namespace Fizzy.McapSharp.Tests;

public class ConvenienceTests
{
    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void ClassifiedRecordsOwnDataAndPreserveDuplicates(McapCompression compression)
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { Compression = compression, ChunkSize = 1024 }, true))
        {
            var schema = writer.RegisterSchema("s", "raw", [1, 2]);
            var channel = writer.RegisterChannel("t", "raw", schema);
            writer.RegisterChannel("unused", "raw", schema);
            for (uint i = 0; i < 3; i++)
            {
                writer.WriteMessage(new(channel, i, i, i), new byte[70000]);
                writer.WriteMetadata("m", new Dictionary<string, string> { ["k"] = i.ToString() });
                writer.WriteAttachment("a", "raw", i, i, i == 0 ? [] : new byte[] { (byte)i });
            }
            writer.Complete();
        }
        var bytes = storage.ToArray();
        McapSchema[] schemas;
        McapAttachment[] attachments;
        using (var session = McapFileReader.OpenRecords(new MemoryStream(bytes))) schemas = session.ReadSchemas().ToArray();
        using (var session = McapFileReader.OpenRecords(new MemoryStream(bytes))) attachments = session.ReadAttachments().ToArray();
        Assert.Equal(new byte[] { 1, 2 }, Assert.Single(schemas).Data);
        Assert.Equal(3, attachments.Length);
        Assert.Empty(attachments[0].Data);
        Assert.Equal(new byte[] { 1 }, attachments[1].Data);
        Assert.Equal(new byte[] { 2 }, attachments[2].Data);
        using (var session = McapFileReader.OpenRecords(new MemoryStream(bytes)))
            Assert.Equal(new[] { "t", "unused" }, session.ReadChannels().Select(c => c.Topic));
        using (var session = McapFileReader.OpenRecords(new MemoryStream(bytes)))
            Assert.Equal(new[] { "0", "1", "2" }, session.ReadMetadata().Select(m => m.Values["k"]));
        using (var session = McapFileReader.OpenRecords(new MemoryStream(bytes)))
        {
            using (var enumerator = session.ReadMetadata().GetEnumerator()) Assert.True(enumerator.MoveNext());
            Assert.Equal(new[] { "1", "2" }, session.ReadMetadata().Select(m => m.Values["k"]));
        }
        bytes[^1] ^= 1;
        using var damaged = McapFileReader.OpenRecords(new MemoryStream(bytes), options: McapReaderOptions.Strict);
        Assert.Throws<McapException>(() => damaged.ReadMetadata().ToArray());
    }

    [Theory]
    [InlineData(1u)]
    [InlineData(7u)]
    public unsafe void RemovedSnapshotOperationsAreRejected(uint operation)
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, leaveOpen: true)) writer.Complete();
        byte[] data = storage.ToArray();
        fixed (byte* p = data)
        {
            var status = Native.fm_snapshot_bytes(p, (nuint)data.Length, out var pointer, out var result);
            Native.Consume(status, result).Json?.Dispose();
            using var handle = new SnapshotHandle(pointer);
            status = Native.fm_snapshot_call(handle, operation, null, 0, 0, 0, null, 0, out _, out result);
            Assert.True(status < 0);
            Assert.Contains("Unknown snapshot operation", Native.ConsumeError(result).Message);
        }
    }
}
