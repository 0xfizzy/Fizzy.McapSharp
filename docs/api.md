# API, lifetime, and data ownership

English | [简体中文](api.zh-CN.md)

Public types are in the `Fizzy.McapSharp` namespace. Execution requires .NET 8 and a Windows x64 process. APIs accept file paths; there are no Stream, async, or cancellation interfaces. Applications define payload encoding, schema contents, and clock semantics; the library does not decode business data.

## Writing files

`McapWriter(path, options)` creates a new file and refuses to overwrite an existing path. Register the schema and channel needed by each message before writing it; additional channels can be registered during recording. IDs are assigned by the implementation, and equivalent registrations may return an existing ID. Do not assume IDs are consecutive or reusable across files.

| Method | Contract |
| --- | --- |
| `RegisterSchema(name, encoding, data)` | Returns a `ushort` schema ID; data contains raw bytes. |
| `RegisterChannel(topic, messageEncoding, schemaId = 0, metadata = null)` | Returns a `ushort` channel ID; schemaId 0 means no schema. |
| `WriteMessage(channelId, logTime, publishTime, sequence, data)` | Requires a registered channel. The caller supplies `ulong` timestamps in nanoseconds and a `uint` sequence number. |
| `WriteMetadata(name, metadata)` | Writes a name and string key/value pairs. |
| `WriteAttachment(name, mediaType, logTime, createTime, data)` | Writes an attachment with timestamps and media type. |
| `Flush()` | Calls the underlying flush; does not finish the file or provide Complete's disk synchronization. |
| `Complete()` | Finishes the MCAP file and synchronizes it to disk. Repeated calls after success do nothing; other write operations are rejected. |
| `Dispose()` | Releases resources without finishing the recording. Treat files without a successful Complete as incomplete. |

An internal lock serializes writer operations; applications still coordinate ordering between threads. Input spans are copied and consumed before the call returns, so their source buffers can then be reused. Errors during native calls or response handling make the writer terminal. Subsequent operations throw `InvalidOperationException`; dispose the instance and start a new recording at a new path instead of retrying writes on it.

### Writer options

| `McapWriterOptions` property | Default | Meaning |
| --- | --- | --- |
| `Compression` | `McapCompression.None` | None, Lz4, or Zstd; compression applies to chunks. |
| `ChunkSize` | `4 * 1024 * 1024` | Target chunk size in bytes; must be positive. This is not a file or message size limit. |
| `UseChunks` | `true` | Write messages into chunks. |
| `EmitIndexes` | `true` | Controls summary records, message indexes, and chunk indexes. Sequential reading remains available when disabled. |
| `Profile` | Empty string | Writes the MCAP profile without checking application constraints. |

The writer enables chunk, data, summary, and attachment CRC calculation supported by the underlying implementation. Which sections exist depends on options and content.

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"recording-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path, new() { Compression = McapCompression.Zstd }))
{
    var schema = writer.RegisterSchema("sample", "jsonschema", "{}"u8);
    var channel = writer.RegisterChannel("/sample", "json", schema);
    writer.WriteMessage(channel, 1_000, 900, 0, "{\"value\":42}"u8);
    writer.WriteMetadata("session", new Dictionary<string, string> { ["clock"] = "application" });
    writer.WriteAttachment("note.txt", "text/plain", 1_000, 900, "example"u8);
    writer.Complete();
}
```

## Reading and querying

`McapReader(path)` checks the platform and ABI and stores the absolute path; it does not read the file during construction. Each enumeration opens an independent native reader. File errors generally appear when enumeration begins or advances. The reader itself does not implement `IDisposable`; dispose enumerators, which `foreach` does automatically, including when using break.

```csharp
var reader = new McapReader(path);
foreach (var message in reader.ReadMessages(new()
{
    Topic = "/sample", StartTime = 1_000, EndTime = 2_000
}))
    Console.WriteLine($"{message.Channel.Topic}: {message.Data.Length} bytes");
```

- `Topic = null` selects all topics; otherwise matching uses the entire string, without wildcards.
- Time filtering uses `LogTime` and the interval `[StartTime, EndTime)`. A null boundary is unbounded. Start greater than end throws `ArgumentException`; equal boundaries select an empty interval.
- Results follow file/chunk order, without timestamp sorting. Usable indexes select chunks overlapping the time range; missing usable indexes or messages outside chunks cause sequential filtering.
- `ReadSchemas()` and `ReadChannels()` include declarations with no messages and deduplicate by ID. A channel's Schema may be null. `ReadChannels()` enumerates schemas before enumerating channels.
- `ReadMetadata()` and `ReadAttachments()` enumerate their respective records independently of `McapQuery`.

Message, schema, and attachment `Data` properties are managed `byte[]` instances that remain valid after the enumerator is disposed. Arrays are mutable. Reading allocates managed memory; large records can require large allocations or exceed managed array limits.

During enumeration, Windows file sharing denies ordinary writes and deletion. Callers must still keep files stable throughout reading, especially between separate enumerations. Writable mappings created before opening the file are outside this protection.

## Validation and recovery

`Validate()` scans the entire file, checking record parsing, present chunk/attachment/data/summary CRCs, record framing, and final magic. It returns a `ulong` count of records observed by the validation scan, **not a message count**. Failure throws `McapException`. Under MCAP, CRC 0 means no checksum is provided; it cannot establish integrity for the corresponding content. Validation does not check business schema semantics.

Normal reading rejects incomplete files, but queries only check visited chunks and parsed records. A successful query does not prove whole-file integrity. Call `Validate()` explicitly when full validation is required.

`RecoverMessages(accept)` recovers a valid message prefix without modifying the source or skipping damaged records or chunks:

```csharp
var recovered = new List<McapMessage>();
var result = new McapReader(path).RecoverMessages(recovered.Add);
if (!result.IsComplete)
    Console.WriteLine($"Recovered {result.RecoveredMessageCount} messages: {result.Error}");
```

Recovery stops at a read error and runs full validation after enumeration finishes. Always inspect `IsComplete`; receiving messages does not mean the file is complete. `RecoveredMessageCount` counts successful callbacks, and `Error` is null on complete success. Callback exceptions propagate instead of becoming recovery results; earlier callbacks are not rolled back. Buffered content never written to the file cannot be recovered.

## Errors

| Condition | Result |
| --- | --- |
| Non-Windows or non-x64 process | `PlatformNotSupportedException` |
| Missing or unloadable native DLL, or missing entry point | Original .NET native loader exception |
| ABI mismatch, native file error, or MCAP operation failure | `McapException`, derived from `IOException` |
| Invalid arguments caught by validation | Standard argument exceptions |
| Completed, failed, or disposed writer | `InvalidOperationException` or `ObjectDisposedException` |

Allocation, managed deserialization, and callback exceptions are not guaranteed to be wrapped in `McapException`. See [native.md](native.md) for native boundaries.
