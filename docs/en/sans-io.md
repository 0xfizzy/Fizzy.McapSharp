# Drive Sans-I/O yourself

English | [简体中文](../zh-CN/sans-io.md)

Use these protocols only when the application must perform I/O itself. The [runnable source](../../samples/Documentation/SansIoExamples.cs) executes all three loops. The helpers below assume a complete MCAP begins at source position zero; for a prefixed source, translate physical positions to/from that origin. The sample expects two messages. No loop establishes full-file validation merely by reaching `End`.

Respond to the pending request before asking for another event. `SupplyInput` accepts short reads and treats empty input as EOF; it only responds to `Read`. `InsertChunkData` requires the entire requested compressed-data range, excluding the Chunk header, so complete short reads before calling. `BufferTooSmall` retains output for retry. Wrong responses and parsing failures terminate the parser; dispose it rather than retrying arbitrary failures.

The methods below use `using Fizzy.McapSharp;` and the .NET project's default implicit usings. Open the file and call `ReadLinear(input)` for linear reading; reopen the file or rewind before running another loop.

## Linear records

A destination holds raw record bodies. Grow it on `BufferTooSmall`; parse explicitly when typed records are needed.

```csharp
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
```

## Summary requests

Summary returns only `Read`/`Seek`/`End`, with no output buffer. Begin uses `AbsolutePosition`; Current/`End` use `RelativeOffset`. The completed reader is returned to the caller, who must dispose it.

```csharp
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
```

## Indexed messages

After `using var summary = ReadSummary(input)`, use `using var indexed = summary.CreateIndexed()` and call `ReadIndexed(input, indexed)`. Its only input event is `ReadChunk`; message output uses a caller buffer.

```csharp
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
```

See [API contracts](api.md#direct-reader-adapters) for capability and failure boundaries, and [testing](testing.md) for validation.
