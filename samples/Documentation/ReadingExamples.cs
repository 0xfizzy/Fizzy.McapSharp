using Fizzy.McapSharp;

internal static class ReadingExamples
{
    internal static void Queries(string path)
    {
        var factory = new McapReaderFactory(path);
        foreach (var message in factory.ReadMessages(new() { Topic = "/sample" }))
            Console.WriteLine(message.LogTime);

        using var query = factory.QueryMessages(new()
        {
            Topic = "/sample",
            Order = McapReadOrder.LogTime,
            AllowBufferedSort = false
        });
        foreach (var message in query.ReadMessages())
            Console.WriteLine(message.LogTime);

        using var indexed = factory.OpenIndexedMessages(new() { Topic = "/sample" });
        foreach (var message in indexed.ReadMessages())
            Console.WriteLine(message.LogTime);
    }

    internal static void Ownership(string path)
    {
        using var session = new McapReaderFactory(path).OpenMessages();
        byte[] buffer = [];
        while (true)
        {
            var status = session.ReadNext(buffer, out var header, out var length);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                buffer = new byte[checked((int)length)];
                continue;
            }
            Console.WriteLine($"{header.LogTime}: {System.Text.Encoding.UTF8.GetString(buffer, 0, checked((int)length))}");
        }

        using var borrowed = new McapReaderFactory(path).OpenMessages();
        borrowed.VisitMessages((in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
        {
            Console.WriteLine($"{header.LogTime}: {payload.Length} bytes");
            return true;
        });

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
    }

    internal static void Streams(string path)
    {
        using var input = File.OpenRead(path);
        using (var session = McapReaderFactory.OpenMessages(input, leaveOpen: true))
        {
            foreach (var message in session.ReadMessages())
                Console.WriteLine(message.LogTime);
        }
        input.Position = 0; // The session has released the Stream; the caller can reuse it.
    }

    internal static async Task Async(string path)
    {
        await using var input = new FileStream(path, FileMode.Open, FileAccess.Read,
            FileShare.Read, 4096, FileOptions.Asynchronous);
        await using (var reader = new McapAsyncMessageReader(input, leaveOpen: true))
        {
            while (await reader.ReadBatchLeaseAsync() is { } lease)
            {
                using (lease)
                {
                    for (int i = 0; i < lease.Count; i++)
                        Console.WriteLine(lease.GetHeader(i).LogTime);
                }
            }
        }
    }

}
