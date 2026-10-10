using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"mcap-docs-{Guid.NewGuid():N}.mcap");
try
{
    // 1. Complete the format explicitly. Disposal only releases resources.
    using (var writer = new McapWriter(path))
    {
        var channel = writer.RegisterChannel("/sample", "json");
        writer.WriteMessage(new McapMessageHeader(channel, 0, 100, 100), "{\"value\":1}"u8);
        writer.Complete();
    }
    var factory = new McapReaderFactory(path);
    factory.Validate();
    if (factory.ReadMessages().Count() != 1) throw new Exception("Owned reading failed");

    // 2. The pending record survives a too-small destination. Growth is caller-owned.
    using (var session = factory.OpenMessages())
    {
        byte[] buffer = [];
        var count = 0;
        while (true)
        {
            var status = session.ReadNext(buffer, out _, out var length);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)length)];
                continue;
            }
            if (length != 11) throw new Exception("Unexpected payload length");
            count++;
        }
        if (count != 1) throw new Exception("Buffer retry lost a record");
    }

    // 3. File order streams without collecting a global time sort.
    using (var query = factory.OpenMessages(new() { Topic = "/sample", Order = McapReadOrder.File, AllowBufferedSort = false }))
    {
        var count = 0;
        query.VisitMessages((in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
        {
            // Borrowed payload expires on return; do not retain this span.
            if (header.LogTime != 100 || payload.Length != 11) throw new Exception("Borrowed result mismatch");
            count++;
            return true;
        });
        if (count != 1) throw new Exception("Query mismatch");
    }

    // Time ordering on an unindexed file must not silently collect data when prohibited.
    using (var unindexed = new MemoryStream())
    {
        using (var writer = new McapWriter(unindexed, new() { UseChunks = false, EmitSummaryRecords = false }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("/sample", "json");
            writer.WriteMessage(new McapMessageHeader(channel, 0, 100, 100), "{}"u8);
            writer.Complete();
        }
        unindexed.Position = 0;
        var rejected = false;
        try
        {
            using var query = McapReaderFactory.OpenMessages(unindexed,
                new() { Order = McapReadOrder.LogTime, AllowBufferedSort = false }, leaveOpen: true);
        }
        catch (NotSupportedException) { rejected = true; }
        if (!rejected) throw new Exception("Unindexed time sorting was not rejected");
    }

    // 4. A retained batch survives reader disposal and forwards synchronously.
    McapMessageBatchLease batch;
    using (var session = factory.OpenMessages()) batch = session.ReadBatchLease()!;
    using (batch)
    using (var stream = new MemoryStream())
    {
        using (var writer = new McapWriter(stream, leaveOpen: true))
        {
            writer.RegisterChannel(batch.GetHeader(0).ChannelId, "/sample", "json");
            if (writer.WriteBatch(batch) != 1) throw new Exception("Forwarding failed");
            writer.Complete();
        }
        stream.Position = 0;
        using var session = McapReaderFactory.OpenMessages(stream, leaveOpen: true);
        if (session.ReadMessages().Count() != 1) throw new Exception("Stream ownership example failed");
    }

    // 5. Consume each ValueTask before another operation or disposal.
    await using var input = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, FileOptions.Asynchronous);
    await using (var reader = new McapAsyncReader(input, leaveOpen: true))
    {
        var count = 0;
        while (await reader.ReadBatchLeaseAsync() is { } lease)
        {
            using (lease) count += lease.Count;
        }
        if (count != 1) throw new Exception("Async reading failed");
    }
    Console.WriteLine("Documentation examples passed.");
}
finally { File.Delete(path); }
