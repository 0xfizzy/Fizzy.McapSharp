# Stream input/output and asynchronous reading

<a id="streams-and-asynchronous-reading"></a>

English | [简体中文](../zh-CN/streams-async.md)

For applications supplying output/input Streams or needing asynchronous reading. The examples use the `path` from the [tutorial](usage.md). Keep Stream ownership separate from the [ownership of delivered messages](ownership.md).

## Supply a Stream to the writer

Choose creation behavior first: `FileMode.CreateNew` rejects existing files; `FileMode.Create` truncates immediately on opening. Seekable output must be at its end. This example uses a new independent path and retains final cleanup responsibility in the caller:

```csharp
using var output = new FileStream(outputPath, FileMode.CreateNew, FileAccess.Write);
using (var writer = new McapWriter(output, leaveOpen: true))
{
    var channel = writer.RegisterChannel("/sample", "raw");
    writer.WriteToKnownChannel(new(channel, 0, 0, 0), []);
    writer.Complete();
    writer.FlushToDisk();
}
// The caller still owns output; dispose it when finished.
```

Writer calls consume input synchronously; there is no asynchronous writing session. With default `leaveOpen: false`, the writer releases the Stream; with true, the caller does. Do not externally write, reposition or dispose the Stream while the writer holds it. `IntoInner()` can transfer ownership but does not implicitly complete or synchronize the file. See the [writing guide](writing.md) for completion rules.

## Retain a caller-owned Stream

```csharp
using var input = File.OpenRead(path);
using (var session = McapReaderFactory.OpenMessages(input, leaveOpen: true))
{
    foreach (var message in session.ReadMessages())
        Console.WriteLine(message.LogTime);
}
input.Position = 0; // The session has released the Stream; the caller can reuse it.
```

The initial position becomes MCAP offset zero; input must contain one MCAP from that position to its end. The session reads incrementally. `leaveOpen` defaults to false, so set it to true only when the caller will own subsequent use and cleanup. Do not read, reposition or dispose the Stream externally while a session owns it.

## Await one operation at a time

```csharp
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
```

Run this as a separate example or rename `input` if combining it with the previous snippet. Consume each `ValueTask` exactly once before starting another operation, looking up declarations, transferring ownership or disposing the reader. Dispose each delivered lease after use. The message reader does not sort queries; `McapAsyncRecordReader` is the separate caller-buffer record capability.

Batch leases may allocate control objects. A nonempty async batch can return at the next input request before reaching its target size. Async I/O does not imply zero allocations from arbitrary Streams or caller code; see [asynchronous contracts](api.md#asynchronous-records-and-errors) and [batch contracts](api.md#borrowing-batches-and-leases).

## Cancellation and cleanup

Pass a cancellation token through the reading API when needed. Cancellation, parsing failures and I/O failures terminate that reader; dispose it rather than retrying arbitrary failed operations. Already delivered leases remain valid until their owners release them.

Finish and consume the outstanding operation before disposal. Reader `DisposeAsync()` performs synchronous release, including `Dispose()` on an owned Stream; it does not drain pending work or call that Stream's `DisposeAsync()`. In the example, `leaveOpen: true` leaves final asynchronous Stream disposal to the outer caller-owned scope.

Explicit cleanup failures are observable. Do not treat an exception as proof that the Stream is safe to reuse. See [Stream release and transfer contracts](api.md#stream-sessions-and-ownership) for one-shot cleanup and ownership boundaries.

Next, use [Sans-I/O](sans-io.md) only if the application needs to drive I/O itself; otherwise consult [API contracts](api.md) for the selected reader.

These snippets run in the [reading examples](../../samples/Documentation/ReadingExamples.cs); see the [tutorial](usage.md#run-repository-examples) for execution instructions.

The output example is verified in the [writing examples](../../samples/Documentation/WritingExamples.cs).
