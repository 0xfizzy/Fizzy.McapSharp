using Fizzy.McapSharp;

// Each loop drives a distinct public protocol. The file's MCAP origin is zero.
static class SansIoExamples
{
    internal static void Run(string path)
    {
        using var file = File.OpenRead(path);
        ReadLinear(file);
        file.Position = 0;
        using var summary = ReadSummary(file);
        using var indexed = summary.CreateIndexed();
        ReadIndexed(file, indexed);
    }

    static void ReadLinear(Stream input)
    {
        using var reader = new McapSansIoLinearReader();
        byte[] io = new byte[4096], record = [];
        while (true)
        {
            var status = reader.NextEvent(record, out var item);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                record = new byte[checked((int)item.Length)];
                continue;
            }
            if (item.Kind == McapLinearEventKind.Read)
            {
                int count = input.Read(io, 0, (int)Math.Min((ulong)io.Length, item.Length));
                reader.SupplyInput(io.AsSpan(0, count)); // A short read is valid; zero signals EOF.
            }
            else
            {
                // Direct linear events are raw: parse explicitly when interpretation is needed.
                _ = McapRecords.Parse(item.Opcode, record.AsSpan(0, checked((int)item.Length)));
            }
        }
    }

    static McapSansIoSummaryReader ReadSummary(Stream input)
    {
        var reader = new McapSansIoSummaryReader();
        try
        {
            byte[] io = new byte[4096];
            while (reader.NextEvent(out var item) != McapReadStatus.EndOfStream)
            {
                if (item.Kind == McapSummaryEventKind.Seek)
                {
                    var seek = item.Seek;
                    long offset = seek.Origin == SeekOrigin.Begin
                        ? checked((long)seek.AbsolutePosition) : seek.RelativeOffset;
                    reader.NotifySeeked(checked((ulong)input.Seek(offset, seek.Origin)));
                }
                else
                {
                    int count = input.Read(io, 0, (int)Math.Min((ulong)io.Length, item.Length));
                    reader.SupplyInput(io.AsSpan(0, count));
                }
            }
            return reader; // The caller owns the completed parser.
        }
        catch (Exception operation)
        {
            try { reader.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }

    static void ReadIndexed(Stream input, McapSansIoIndexedReader reader)
    {
        byte[] chunk = [], payload = [];
        int messages = 0;
        while (true)
        {
            var status = reader.NextEvent(payload, out var item);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                payload = new byte[checked((int)item.Length)];
                continue;
            }
            if (item.Kind == McapIndexedEventKind.ReadChunk)
            {
                int length = checked((int)item.Length);
                if (chunk.Length < length) chunk = new byte[length];
                input.Position = checked((long)item.Offset);
                input.ReadExactly(chunk.AsSpan(0, length)); // Complete short reads before submission.
                reader.InsertChunkData(item.Offset, chunk.AsSpan(0, length));
            }
            else
            {
                // Use item.Header and payload[..Length] before reusing the destination.
                messages++;
            }
        }
        if (messages != 2) throw new Exception("Indexed protocol example lost messages.");
    }
}
