using Fizzy.McapSharp;

internal static class ReadingExamples
{
    internal static void Queries(string path)
    {
        var factory = new McapReaderFactory(path);
        foreach (var message in factory.ReadMessages(new() { Topic = "/sample" }))
            Console.WriteLine(message.LogTime);

        if (!factory.ReadMessages().Select(m => m.LogTime).SequenceEqual(new ulong[] { 100, 50 }) ||
            !factory.ReadMessages(new() { Topic = "/sample" }).Select(m => m.LogTime).SequenceEqual(new ulong[] { 100, 50 }) ||
            !factory.ReadQueryMessages(new() { Topic = "/sample" }).Select(m => m.LogTime).SequenceEqual(new ulong[] { 50, 100 }))
            throw new Exception("Sequential or default query order mismatch.");

        using var query = factory.QueryMessages(new()
        {
            Topic = "/sample",
            Order = McapReadOrder.LogTime,
            AllowBufferedSort = false
        });
        var ordered = query.ReadMessages().ToArray();
        foreach (var message in ordered) Console.WriteLine(message.LogTime);
        if (!ordered.Select(m => m.LogTime).SequenceEqual(new ulong[] { 50, 100 }))
            throw new Exception("Explicit query order mismatch.");

        using var indexed = factory.OpenIndexedMessages(new() { Topic = "/sample" });
        if (!indexed.ReadMessages().Select(m => m.LogTime).SequenceEqual(new ulong[] { 50, 100 }))
            throw new Exception("Indexed order mismatch.");
    }

    internal static void Ownership(string path)
    {
        using var session = new McapReaderFactory(path).OpenMessages();
        byte[] buffer = [];
        int count = 0, retries = 0;
        while (true)
        {
            var status = session.ReadNext(buffer, out var header, out var length);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                retries++;
                buffer = new byte[checked((int)length)];
                continue;
            }
            if (length != 11 || header.LogTime != (count == 0 ? 100UL : 50UL))
                throw new Exception("Buffer result mismatch.");
            count++;
            Console.WriteLine($"{header.LogTime}: {System.Text.Encoding.UTF8.GetString(buffer, 0, checked((int)length))}");
        }

        if (count != 2 || retries == 0) throw new Exception("Buffer negotiation mismatch.");
        int visited = 0;
        using var borrowed = new McapReaderFactory(path).OpenMessages();
        borrowed.VisitMessages((in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
        {
            if (header.LogTime != (visited == 0 ? 100UL : 50UL) || payload.Length != 11)
                throw new Exception("Borrowed result mismatch.");
            visited++;
            Console.WriteLine($"{header.LogTime}: {payload.Length} bytes");
            return true;
        });

        if (visited != 2) throw new Exception("Borrowed count mismatch.");
        using var retained = new McapReaderFactory(path).OpenMessages();
        byte[]? savedPayload = null;
        using (var batch = retained.ReadBatchLease(1))
        {
            if (batch is not null)
            {
                savedPayload = new byte[batch.GetPayload(0).Length];
                batch.CopyTo(0, savedPayload);
            }
        }
        // savedPayload is independent of the lease and reader.
        if (savedPayload is null || !savedPayload.AsSpan().SequenceEqual("{\"value\":1}"u8))
            throw new Exception("Independent copy mismatch.");
    }

    internal static void Streams(string path)
    {
        using var input = File.OpenRead(path);
        using (var session = McapReaderFactory.OpenMessages(input, leaveOpen: true))
        {
            if (session.ReadMessages().Count() != 2) throw new Exception("Stream read mismatch.");
        }
        input.Position = 0; // The session has released the Stream; the caller can reuse it.
    }

    internal static async Task Async(string path)
    {
        await using var input = new FileStream(path, FileMode.Open, FileAccess.Read,
            FileShare.Read, 4096, FileOptions.Asynchronous);
        await using (var reader = new McapAsyncMessageReader(input, leaveOpen: true))
        {
            int count = 0;
            while (await reader.ReadBatchLeaseAsync() is { } lease)
            {
                using (lease)
                {
                    count += lease.Count;
                    for (int i = 0; i < lease.Count; i++)
                        Console.WriteLine(lease.GetHeader(i).LogTime);
                }
            }
            if (count != 2) throw new Exception("Async count mismatch.");
        }
    }

}
