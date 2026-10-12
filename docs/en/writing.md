# Write files

<a id="writing-a-recording"></a>

English | [简体中文](../zh-CN/writing.md)

Start by saving messages from several topics, then add metadata, attachments or other writing forms as needed. See the [tutorial](usage.md) for project setup and [Read messages](reading.md) to read the resulting file.

## Write a multi-topic file

Run this C# 12 program in the project from the tutorial. Keep the printed path for the reading guide; delete the file when no longer needed.

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"recording-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var schema = writer.RegisterSchema("value", "jsonschema", "{\"type\":\"object\"}"u8);
    var values = writer.RegisterChannel("/values", "json", schema);
    var events = writer.RegisterChannel("/events", "text/plain");
    writer.WriteToKnownChannel(new(values, 0, 100, 90), "{\"value\":1}"u8);
    writer.WriteToKnownChannel(new(events, 0, 50, 50), "started"u8);
    writer.WriteToKnownChannel(new(events, 1, 110, 110), []);
    writer.Complete();
}
Console.WriteLine(path);
```

The messages are deliberately written at log times 100, 50 and 110; writing does not sort them. The final message has an empty payload and is still a message. Payload input is consumed before the call returns, so caller buffers can then be reused.

The destination path must not exist. Call `Complete()` to finish the file; leaving `using` only releases resources. Log and publish timestamps in the message header are nanoseconds with an application-defined origin.

## Understand declarations and messages

A Schema describes a payload format; a Channel associates a topic and message encoding with an optional Schema and metadata. Register a Schema once, then use its ID when registering related channels. Schema ID zero means no schema. A message header identifies its channel and carries sequence, log time and publish time; its payload is supplied separately.

Applications define payload encoding, sequence policy and the origins of nanosecond timestamps. The library does not serialize application objects or check payloads against schemas. IDs belong to the recording: when forwarding into another file, register destination declarations and map IDs as needed.

## Include metadata and attachments

Insert the following code before `writer.Complete()` in the program above, then run it again. The reading guide uses this data for its metadata and attachment examples.

```csharp
writer.WriteMetadata("recording", new Dictionary<string, string> { ["operator"] = "sample" });
writer.WriteAttachment("notes.txt", "text/plain", 100, 90, "notes"u8);
writer.StartAttachment("parts.txt", "text/plain", 100, 90, 6);
writer.WriteAttachmentBytes("abc"u8);
writer.WriteAttachmentBytes("def"u8);
writer.FinishAttachment();
```

Metadata stores named string key/value maps. Attachments store named bytes with a media type and timestamps. Segmentation avoids assembling the entire payload in one caller buffer; it requires the exact total length. Between `StartAttachment` and `FinishAttachment`, supply only that attachment's bytes. Other writer operations are rejected while it is open; a length mismatch is terminal.

Private record writing is limited to opcodes 0x80–0xFF. Reading arbitrary raw records does not imply a general arbitrary-record writer. See [writing contracts](api.md#write-a-recording).

<a id="finish-and-handle-failures"></a>

## Complete, persist and dispose

| Action | Purpose |
| --- | --- |
| `Flush()` while writing | Flush writer output and finish the active chunk; does not finish the file |
| `Complete()` | Finish MCAP structures and ordinarily flush output; call explicitly |
| `FlushToDisk()` after completion | Request file synchronization for path output or a directly supplied FileStream |
| `Dispose()` | Release resources; does not complete the recording |

After successful completion, `GetSummary()` returns an independent copy; `OpenSummaryRecords()` provides a retained summary cursor. Dispose the writer when summary, synchronization and Stream-transfer work is finished. `IntoInner()` transfers a Stream without implicitly completing or synchronizing it. A synchronization request succeeding is not an unconditional hardware persistence guarantee.

## Choose an output

`new McapWriter(path)` creates a new file and rejects an existing path without changing it. To choose truncation or supply another destination, open a writable Stream yourself and pass it to the writer. A seekable output must be positioned at its end; that position becomes MCAP offset zero. Non-seekable outputs use native chunk buffering.

The writer owns a supplied Stream unless `leaveOpen: true`. See [Streams and asynchronous reading](streams-async.md) for a complete ownership example. Writer operations are synchronous and serialized; applications determine message ordering across producers.

## Choose a writing form

| Existing application data | Writing form | Preparation and cost |
| --- | --- | --- |
| Registered channel and reusable bytes | `WriteToKnownChannel(header, span)` | Register once; warmed binding message writes allocate 0 B managed |
| Owned message with complete declarations | `WriteMessage(message)` | Automatic declarations use supplied IDs; observes current input descriptions and may allocate |
| Repeated full-message writes | `WriteMessage(prepared, header, span)` | Snapshot a channel once; declarations occur when writing; header ID must match |
| Headers and payloads already in one storage region | `WriteBatch(headers, storage, ranges)` | Register channels first; do not concatenate payloads just to use batching |
| A retained read batch | `WriteBatch(lease, headers)` | Register destination declarations and replace IDs if needed; retain lease throughout the call |

For automatic declarations, use a complete channel description instead of calling registration separately:

```csharp
using var output = new MemoryStream();
var schema = new McapSchema(1, "value", "jsonschema", "{}"u8.ToArray());
var channel = new McapChannel(7, "/values", "json", schema, new Dictionary<string, string>());
using (var writer = new McapWriter(output, leaveOpen: true))
{
    writer.WriteMessage(new McapMessage(channel, 0, 100, 90, "{}"u8.ToArray()));
    using var prepared = new McapPreparedChannel(channel);
    writer.WriteMessage(prepared, new(channel.Id, 1, 200, 190), "{}"u8);
    writer.Complete();
}
```

This is a separate example. Preparation snapshots schema bytes and metadata; it does not register declarations. Dispose prepared descriptors after their last use. Typed prepared descriptors also exist for explicit registration, metadata and attachments. See [prepared contracts](api.md#complete-messages-and-prepared-writes). Batch operations are not atomic; do not replay an entire failed batch. See [Buffers and message forwarding](ownership.md).

## Handle failures

By default, five audited pre-mutation native rejections permit continuation: invalid schema ID zero, conflicting schema registration, unknown schema on channel registration, conflicting channel registration and unknown channel on header/payload writing. They still throw `McapException`; inspect `CanContinueWriting` and correct the specific input before continuing. Full-message automatic declarations are not covered by that whitelist. Set `SafeRejections = McapWriterSafeRejections.None` to make every native writer failure terminal.

I/O, compression, callback and other unaudited native failures terminate the writer. Dispose it and begin a new recording; do not retry arbitrary failed operations. Managed argument/state checks before native writing do not by themselves terminate it. Explicit cleanup can also fail and must remain visible. Detailed rules are in [safe rejections](api.md#safe-writer-rejections) and [Stream ownership](api.md#stream-sessions-and-ownership).

## Next steps

Choose [Compression, chunks and indexes](writer-options.md), [Read messages](reading.md), or [Buffers and message forwarding](ownership.md). The [writing examples](../../samples/Documentation/WritingExamples.cs) and [recording round trip](../../samples/Documentation/RecordingExamples.cs) execute and check these operations; run them with the [sample instructions](usage.md#run-repository-examples).
