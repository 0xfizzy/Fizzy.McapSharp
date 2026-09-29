using Fizzy.McapSharp;

var folder = Path.GetFullPath(args[1]);
Directory.CreateDirectory(folder);
if (args[0] == "write")
{
    foreach (var compression in Enum.GetValues<McapCompression>())
    {
        var path = Path.Combine(folder, $"dotnet-{compression}.mcap");
        using var stream = compression == McapCompression.None ? null : new FileStream(path, FileMode.CreateNew, FileAccess.Write);
        using var writer = stream is null ? new McapWriter(path, new() { Compression = compression, ChunkSize = 64 }) : new McapWriter(compression == McapCompression.Lz4 ? new NonSeekableOutput(stream) : stream, new() { Compression = compression, ChunkSize = 64 }, leaveOpen: true);
        var schema = writer.RegisterSchema(7, "sample", "jsonschema", "{}"u8);
        var channel = writer.RegisterChannel(9, "/test", "json", schema);
        for (uint n = 0; n < 100; n++)
            writer.WriteMessage(new McapMessageHeader(channel, n, n * 100, n * 100 + 1), System.Text.Encoding.UTF8.GetBytes($"{{\"n\":{n}}}"));
        writer.WriteMetadata("session", new Dictionary<string, string> { { "origin", "dotnet" } });
        writer.StartAttachment("sample", "text/plain", 100, 90, 10);
        writer.WriteAttachmentBytes("attach"u8);
        writer.WriteAttachmentBytes("ment"u8);
        writer.FinishAttachment();
        writer.Complete();
    }
}
else if (args[0] is "read" or "read-all")
{
    var paths = Directory.GetFiles(folder, args[0] == "read-all" ? "*.mcap" : "python-*.mcap", SearchOption.AllDirectories);
    if (paths.Length != (args[0] == "read-all" ? 18 : 3))
        throw new Exception("Missing interoperability fixtures");
    foreach (var path in paths)
    {
        var reader = new McapReader(path);
        reader.Validate();
        var messages = reader.ReadMessages(new() { Topic = "/test", StartTime = 200, EndTime = 500 }).ToArray();
        if (messages.Length != 3 || messages[0].Sequence != 2 || messages[2].Sequence != 4)
            throw new Exception("Interoperability query failed: " + path);
        if (reader.ReadMetadata().Single().Values["origin"] != (Path.GetFileName(path).StartsWith("python-") ? "python" : "dotnet"))
            throw new Exception("Metadata mismatch");
        if (System.Text.Encoding.UTF8.GetString(reader.ReadAttachments().Single().Data) != "attachment")
            throw new Exception("Attachment mismatch");
        using var session = reader.OpenMessages(new() { Topic = "/test", StartTime = 200, EndTime = 500 });
        var buffer = new byte[100];
        int count = 0;
        while (session.ReadNext(buffer, out var header, out _) != McapReadStatus.EndOfStream)
        {
            if (header.Sequence != (uint)(count + 2))
                throw new Exception("Buffered query mismatch");
            count++;
        }

        if (count != 3)
            throw new Exception("Buffered query count");
        Console.WriteLine("Validated " + Path.GetFileName(path));
    }
}
else
    throw new ArgumentException("Expected write, read or read-all");
sealed class NonSeekableOutput(Stream inner) : Stream
{
    public override bool CanRead => false;
    public override bool CanSeek => false;
    public override bool CanWrite => true;
    public override long Length => throw new NotSupportedException();
    public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }

    public override void Flush() => inner.Flush();
    public override void Write(ReadOnlySpan<byte> data) => inner.Write(data);
    public override void Write(byte[] data, int offset, int count) => inner.Write(data, offset, count);
    public override int Read(byte[] data, int offset, int count) => throw new NotSupportedException();
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
    public override void SetLength(long length) => throw new NotSupportedException();
}
