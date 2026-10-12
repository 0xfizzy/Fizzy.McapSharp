# Read messages

<a id="reading-a-recording"></a>

English | [简体中文](../zh-CN/reading.md)

Use `ReadMessages()` to iterate in file order, with an independent copy of each message’s data. This guide reads the file produced by the [Write files](writing.md) guide. See the [tutorial](usage.md) for project setup.

## Read the multi-topic file

Use this as a standalone C# 12 program; run `dotnet run -- <path>` with the path printed by the writing guide.

```csharp
using Fizzy.McapSharp;
using System.Text;

if (args.Length != 1) throw new ArgumentException("Supply the recording path.");
var factory = new McapReaderFactory(args[0]);
foreach (var message in factory.ReadMessages())
    Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

Output follows file order; the last message has an empty payload:

```text
/values 100: {"value":1}
/events 50: started
/events 110:
```

## Understand declarations and messages

`McapMessage` combines a header's sequence and nanosecond timestamps with a resolved Channel and payload bytes. Its Channel includes the optional Schema and metadata. Each message owns a separate, mutable copy of its data; these are called owned results. The library interprets the MCAP container, not the application payload encoding or clock origin.

## Filter by topic and time

Append this to the program above. It prints `/events 50: started`. Filtering uses `LogTime`, includes 50 and excludes 110. Topic or time filters preserve file order; see [queries and random access](queries.md) for time ordering or indexed-only access.

```csharp
foreach (var message in factory.ReadMessages(new()
{
    Topic = "/events",
    StartTime = 50,
    EndTime = 110
}))
    Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

## Read other records and summaries

If you added the metadata and attachments from the writing guide, append this to the reading program. Each factory operation performs its own read. Metadata prints `recording: sample`; attachments contain `notes` and `abcdef`.

```csharp
foreach (var metadata in factory.ReadMetadata())
    Console.WriteLine($"{metadata.Name}: {metadata.Values["operator"]}");
foreach (var attachment in factory.ReadAttachments())
    Console.WriteLine($"{attachment.Name}: {Encoding.UTF8.GetString(attachment.Data)}");
var summary = factory.GetSummary();
Console.WriteLine(summary is null ? "No summary" : "Summary available");
```

Use `ReadSchemas()` and `ReadChannels()` to enumerate declarations, including declarations without messages. `ReadMetadata()` and `ReadAttachments()` return independent copies of the data. An independent `GetSummary()` result includes the available declarations, statistics and indexes, or is null when no summary exists; fetching it does not validate the whole recording.

`OpenRecords()` expands chunks into records; `OpenLinear()` exposes top-level records. Use these when record structure matters instead of resolved messages. `McapRecordAccessor` supplies seekable raw-record access, while an indexed reader supports indexed attachment/metadata access and message seeks. Select by capability rather than expecting every reader to expose every operation. See [record contracts](api.md#owned-records-summaries-and-raw-records) and [Sans-I/O reading](sans-io.md) when the application must drive input requests itself.

## Choose an input and session

`McapReaderFactory(path)` stores a path without opening the file and owns no disposable input. Each enumeration or opened session owns an independent reader. Use `ReadMessages()` for convenient owned results, or dispose the session returned by `OpenMessages()` when choosing buffers, callbacks or leases.

For an existing Stream, use `McapReaderFactory.OpenMessages(stream, leaveOpen: true)` when the caller retains cleanup responsibility. For repeated random access, `McapIndexedReader` owns copied or mapped input and requires disposal; it is not just an index collection. See [Streams and asynchronous reading](streams-async.md) and [random access](queries.md).

<a id="choose-a-delivery-form"></a>

## Choose how to receive messages

| Need | Entry | Caller responsibility |
| --- | --- | --- |
| Independent mutable messages | `ReadMessages()` | Owns copies; ordinary managed lifetime |
| Reuse payload storage | `ReadNext(buffer, ...)` | Grow on `BufferTooSmall`; process bytes before buffer reuse |
| Immediate synchronous processing | `VisitMessages` / `ReadNext(visitor)` | Do not retain spans or reenter the reader from the callback |
| Retain or forward a batch | `ReadBatchLease` | Dispose the lease after all access; do not dispose concurrently |

A storage lease keeps data available after the reader advances; dispose it after use. Access order and delivery ownership are separate decisions. A lease may retain an entire chunk or mapping, including after reader disposal. [Buffers and message forwarding](ownership.md) provides runnable buffer, callback and lease examples and explains their costs. Async message leases are covered in [Streams and asynchronous reading](streams-async.md).

Buffer and callback reads deliver a header and bytes separately. Use the session's `GetChannelDescription` for immutable declarations encountered by that session. Do not assume a declaration found only in the summary has already appeared in a sequential scan. Raw messages, expanded records and top-level records are distinct capabilities with distinct validation scopes.

## Finish and handle failures

`BufferTooSmall` retains the same pending record: provide the reported capacity and retry. A successful zero-byte payload is not EOF. `EndOfStream` ends the selected scan but does not by itself establish full-file validation. Use [Validation and recovery](validation.md) for that separate decision.

Dispose opened sessions and enumerators after use; `foreach` disposes its enumerator. Owned results and independently retained leases keep their documented lifetimes. Parsing, callback and I/O failures terminate the session; repeat work through a new session instead of retrying arbitrary failures. Explicit cleanup failures remain visible. Async readers require consuming their one outstanding operation before starting another or disposing them.

## Next steps

Choose [queries and random access](queries.md), [Buffers and message forwarding](ownership.md), or [Validation and recovery](validation.md). The [recording examples](../../samples/Documentation/RecordingExamples.cs) check messages, declarations, metadata, attachments, summary, buffer retries and EOF against the writer's output. The [reading examples](../../samples/Documentation/ReadingExamples.cs) cover delivery and Stream APIs; run them using the [sample instructions](usage.md#run-repository-examples).
