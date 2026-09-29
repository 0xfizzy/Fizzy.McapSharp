# API, lifetime, and data ownership

English | [简体中文](api.zh-CN.md)

`Fizzy.McapSharp` provides MCAP file, Stream, buffer and Sans-I/O operations on .NET 8: Windows x64 and glibc Linux x64/ARM64, built against Ubuntu 22.04. macOS, musl and 32-bit processes are unsupported. Applications define payload encodings and nanosecond clock semantics. Cancellable asynchronous record reading and time sorting are available; payload decoding remains application-defined. See the [official API coverage map](coverage.md).

## Write a recording

File creation semantics: `McapWriter(path, options)` atomically creates a new file and fails if the path already exists, leaving the existing file unchanged. This is the path overload's default protection against accidental overwrites, not a requirement of the MCAP format.

`McapWriter(stream, options, leaveOpen: false)` accepts writable seekable or non-seekable streams. The caller chooses creation or truncation when opening the stream. To explicitly overwrite a file, open a `FileStream` with `FileMode.Create` and `FileAccess.Write`, then pass it to the writer; opening that stream immediately truncates any existing file. A seekable output must be positioned at its end; its current position becomes MCAP offset zero. Non-seekable output uses native chunk buffering.

```csharp
using Fizzy.McapSharp;

using var writer = new McapWriter(path, new() { Compression = McapCompression.Zstd });
var schema = writer.RegisterSchema("sample", "jsonschema", "{}"u8);
var channel = writer.RegisterChannel("/sample", "json", schema);
var header = new McapMessageHeader(channel, Sequence: 0, LogTime: 1000, PublishTime: 900);
writer.WriteMessage(in header, "{\"value\":42}"u8);
writer.Complete();
```

| Operation | Contract |
| --- | --- |
| `RegisterSchema(name, encoding, data)` | Returns a native-allocated `ushort` ID; overload with leading `id` requests an explicit nonzero ID. Equivalent content can deduplicate. |
| `RegisterChannel(topic, messageEncoding, schemaId, metadata)` | Returns a channel ID; overload with leading `id` supports explicit IDs, including zero. Schema ID zero means no schema. Conflicting content for an existing ID fails. |
| `WriteMessage(in header, data)` | Writes to a registered channel using a message header and payload span. Consumes the input synchronously without a managed payload copy. |
| `WriteMetadata(name, metadata)` | Writes string key/value metadata. |
| `WriteAttachment(name, mediaType, logTime, createTime, data)` | Writes one attachment. |
| `StartAttachment(..., length)`, `WriteAttachmentBytes(data)`, `FinishAttachment()` | Writes an attachment in parts with an exact declared length. Other writer operations are rejected until completion. Length mismatch is terminal. |
| `WritePrivateRecord(opcode, data, includeInChunks)` | Accepts opcodes 0x80–0xFF; optionally writes inside chunks. |
| `Flush()` | Flushes the upstream writer, without completing the MCAP footer or guaranteeing durable storage. |
| `Complete()` | Finishes MCAP once. Files also receive `sync_all`; streams receive `Flush`, without a durability guarantee. Repeated successful calls are no-ops. |
| `GetSummary()` | Copies the upstream finish result after successful Complete; may allocate. |
| `Dispose()` | Releases resources without implicit Complete. |

Writer calls are serialized. Applications determine cross-thread business ordering. Native failures outside the configured safe-rejection whitelist and all Stream callback failures make the writer terminal: dispose it and begin a new recording. Argument/state checks before native operations do not by themselves fail a writer. Input buffers can be reused immediately after return.

### Writer options

| Option | Default / meaning |
| --- | --- |
| `Compression`, `ChunkSize`, `UseChunks` | Zstd, 1 MiB, true. ChunkSize follows upstream semantics, including zero; null disables the target-size cutoff. |
| `Profile`, `Library` | Empty profile; null Library selects the official Rust library identifier. |
| `EmitSummaryOffsets`, `EmitStatistics` | true; independently control summary offsets and statistics. |
| `EmitMessageIndexes`, `EmitChunkIndexes`, `EmitAttachmentIndexes`, `EmitMetadataIndexes` | true; independently control each index type. |
| `RepeatChannels`, `RepeatSchemas` | null; upstream defaults (true). Control declarations repeated in the summary. |
| `CalculateChunkCrcs`, `CalculateDataSectionCrc`, `CalculateSummarySectionCrc`, `CalculateAttachmentCrcs` | true; independently control CRC computation. |
| `CompressionLevel`, `CompressionThreads` | null; upstream defaults and algorithm support. |

Nullable flags use upstream behavior when null. `EmitSummaryRecords` applies first; explicitly supplied individual switches override it. `DisableSeeking` defaults to false for seekable output and true for non-seekable output; explicit false on non-seekable output fails. To omit summary records, disable statistics, chunk/attachment/metadata indexes and repeated declarations. Disable summary offsets separately. No chunks means no chunk compression or chunk message indexes, regardless of their requested settings. Options do not impose a unified memory quota.

## Read messages into reusable buffers

A file `McapReader` is a factory; constructing it does not open the file. Every `OpenMessages` or `OpenRecords` call owns a separate disposable `McapReadSession` and native handle. Dispose sessions or use `using`.

```csharp
using var session = new McapReader(path).OpenMessages(new() { Topic = "/sample" });
byte[] buffer = new byte[64 * 1024];
while (true)
{
    var status = session.ReadNext(buffer, out var header, out var length);
    if (status == McapReadStatus.EndOfStream) break;
    if (status == McapReadStatus.BufferTooSmall)
    {
        buffer = new byte[checked((int)length)]; // caller-chosen growth, outside zero-allocation contract
        continue;
    }
    // Process buffer.AsSpan(0, checked((int)length)) before reusing it.
}
```

`BufferTooSmall` returns the required payload length and message header, leaves the destination untouched and keeps the same record pending. Successful empty messages return `Message` with length zero. EOF is repeatable. Errors terminate message/record advancement. The API exposes no native addresses or borrowed spans.

Queries accept either one exact `Topic` or a `Topics` collection and apply `[StartTime, EndTime)` to LogTime. Null boundaries are unbounded; reversed boundaries throw, equal boundaries select nothing. Queries default to `LogTime` order, with `ReverseLogTime` and `File` available. Equal-time messages follow file order, reversed for reverse order. Opening messages without a query uses sequential file order, matching the official message stream.

Seekable queries use the official `IndexedReader` when summary declarations and chunk coverage suffice and there are no top-level messages; otherwise they scan. Sorted fallback collects selected messages in native memory before returning the session, including on non-seekable sources. Set `McapQuery.AllowBufferedSort = false` to reject this fallback with NotSupportedException before collecting messages; the default is true. File-order scans and suitable indexed queries remain available. This option does not bound indexed Chunk buffering or suppress index probing. Direct Sans-I/O indexed readers never use this fallback. File-order scans remain incremental. `OpenIndexedMessages` rejects incomplete indexes. Non-default linear parser flags force high-level queries to scan; explicit indexed reading rejects these unsupported flags.

`GetChannel(id)` and `GetSchema(id)` copy descriptions already encountered or loaded from a summary. New declarations can appear during reading without creating managed objects in the message loop. IDs alone are returned on the hot path. Description lookup and summary operations allocate.

## Owned records, summaries and raw records

`ReadMessages`, `ReadSchemas`, `ReadChannels`, `ReadMetadata` and `ReadAttachments` provide convenient owned records. File-factory methods open independent sessions. Session convenience methods consume the current cursor; schema/channel/metadata/attachment methods require a record session. Message data and schema/attachment data are managed arrays and remain valid after disposal. Arrays are mutable. Message enumeration caches channel descriptions within that enumeration, so channel/schema objects may be shared between its messages.

`GetSummary()` returns a managed snapshot of statistics, chunk/attachment/metadata indexes and schema/channel IDs, or null if the file has no summary. Retrieve full declarations with session lookup methods. A writer's finish summary describes its in-memory recording result, even when summary records were disabled in the output.

`OpenRecords(McapRecordMode.TopLevel)` yields top-level records including encoded Chunk bodies. `ExpandChunks` replaces chunks with their decompressed records. `ReadNextRecord(destination, out opcode, out length)` follows the same retry contract as messages; `ReadRecords()` returns allocated `McapRecord` objects. The body excludes the opcode and eight-byte length prefix. Unknown/private record bodies are preserved.

On seekable sources, `ReadRecordAt(offset)`, `ReadChunk(index)` and `ReadMessageIndexes(index)` perform random reads without consuming the sequential cursor or pending record. Offsets are relative to the MCAP start. `ReadChunk` returns the raw Chunk record; `OpenRecords(ExpandChunks)` provides sequential decompression. Index entries contain chunk-relative message offsets. Raw random reads do not validate the whole file.

## Stream sessions and ownership

Use `McapReader.OpenMessages(stream, query, leaveOpen)` or `OpenRecords(stream, mode, leaveOpen)` for Stream input. The current stream position is MCAP offset zero. Sessions consume streams incrementally without a whole-stream copy or temporary file. A stream must contain exactly one MCAP from that position to its end.

Only one MCAP session can own a Stream at a time. Do not reposition, truncate, read, write or dispose it externally while the session is active. Stream callbacks run synchronously on the initiating thread; reentry into the same writer/session is rejected. Callback exceptions propagate after leaving native code. `leaveOpen` defaults to false; disposing a session closes its stream unless true.

Non-seekable streams do not support random reads or an early summary: those calls throw `NotSupportedException`. After a complete sequential scan, `GetSummary` returns the actual summary encountered, including null when absent. To restart or change reading modes, dispose the session and reopen/reposition the source yourself.

Mapped files must remain unchanged across reads, validation and recovery. Windows denies ordinary concurrent write/delete opens while mapped; pre-existing writable mappings are not covered. Linux does not enforce this exclusion. Concurrent truncation of a mapped file can terminate the process.

## Validation, recovery and allocation guarantees

File length is not used as a record or decompressed-chunk size limit. Highly compressed chunks can expand beyond the file size; native memory and upstream parser limits still apply.

`McapReader.Validate()` scans the whole file and checks record parsing, present Chunk/Attachment/Data/Summary CRCs, framing and final magic. It returns a scan record count, not a message count. CRC zero means no checksum was supplied; payload schema semantics are not checked.

`McapReaderOptions` mirrors official Sans-I/O options: magic handling, trailing-byte checks, chunk emission, CRC checks/prevalidation and record length limits. Optional CRC and trailing-byte checks default to false. `IsScanComplete` reports cursor EOF without an integrity claim. `IsComplete` requires a successful full sequential scan with strict options. Open with `McapReaderOptions.Strict` and expanded records/messages to call `ValidateRemaining()`. Non-strict sessions reject this operation rather than claiming to validate previously consumed data. Indexed queries, sorted fallback and raw top-level scans do not claim full validation.

`RecoverMessages(accept)` delivers the valid prefix and stops at the first malformed record or chunk. Check its `IsComplete` and `Error`; delivered messages alone do not establish integrity. Callback exceptions propagate directly and are not recovery results. Buffers never written to the underlying stream cannot be recovered.

After initialization, registration and warm-up, normal `WriteMessage` and buffered `ReadNext` calls are required to allocate exactly zero managed bytes. This hard hot-path contract includes chunk/compression boundaries, newly encountered declarations, insufficient-buffer retries and EOF. This is not zero-copy reading: native data is copied into caller memory. It is not a promise that the process never collects or that Rust never allocates. Buffer growth, owned-record enumeration, description access, startup and errors are outside the guarantee. Arbitrary user Stream implementations can allocate internally; the guarantee covers the library's bridge, not those implementations.

Unsupported platforms throw `PlatformNotSupportedException`; native loading errors retain their .NET type. Native operation/ABI failures throw `McapException` (an `IOException`). Managed parameter/state checks use standard exceptions, and disposed objects throw `ObjectDisposedException`. See [ABI details](native.md) and [allocation acceptance and builds](development.md).


## Complete messages and prepared writes

`WriteMessage(McapMessage)` directly invokes upstream `Writer::write`, including automatic declarations using the supplied IDs. Its zero-allocation counterpart takes `McapPreparedChannel`, a matching message header and a payload span. Preparation snapshots schema bytes and metadata but does not register declarations. The convenience overload observes current mutable inputs on every call; prepared descriptors remain immutable.

`McapPreparedOperation` provides reusable schema, channel, metadata and attachment descriptors. `WritePrepared` executes them without managed allocation and returns registration IDs where applicable. Payloads stay separate spans. Attachment continuation, private records and Flush also have zero-allocation paths. `Finish()` completes and returns an owned Summary. `Complete()` plus `OpenSummaryRecords()` supplies a reusable-buffer summary cursor. `IntoInner()` releases native state and transfers Stream ownership without implicitly completing the file.

## Direct reader adapters

`McapBufferReader` directly adapts official `LinearReader`, `sans_magic`, `ChunkReader`, `ChunkFlattener`, `RawMessageStream` and `MessageStream`. Select `McapBufferReadMode`; Chunk mode accepts a Chunk record body. `ignoreEndMagic` maps to the slice-reader option. Construction copies input once; advancement drives the official Sans-I/O parser and retains one pending result. GetChannel exposes only encountered declarations, including those without messages. Errors are reported when reached; failed advancement leaves previously encountered descriptions available. Use Stream or Sans-I/O sessions for incremental ingestion. `ReadNextRecord` copies record bodies; message modes also provide header/payload `ReadNext`. In RawMessages mode, `GetChannel` retains every declaration successfully encountered by the upstream iterator, including channels without messages and declarations before a deferred error. Descriptions and owned enumeration allocate.

`McapRecords.Parse` returns typed owned models for every standard record and `McapRecord` for unknown records. `McapRecordView.Parse` validates through official `parse_record` and views caller-owned managed memory. Scalar properties and the `Fields` cursor read UTF-8, maps, arrays and binary fields without creating strings/arrays. `ToOwned` copies explicitly. `ReadFooter` and `GetCompressedDataOffset` directly call the upstream helpers.

`OpenIndexSnapshot()` copies a seekable source and reads its official summary without moving the sequential cursor. `new McapIndexSnapshot(bytes)` also accepts caller bytes. The snapshot owns memory proportional to the file, independently of its original session. It exposes `SeekMessage`, `OpenChunkMessages`/`ReadChunkMessages`, message indexes, indexed metadata/attachments, footer, descriptions and summary. Random operations use the supplied index fields, including lengths and channel-offset maps; they do not replace the index with a summary entry having the same offset. Metadata and attachment reads accept caller-provided indexes even without a summary. Chunk message and message-index reads still require summary declarations. Index fields not used by the corresponding upstream helper do not add validation. Do not mutate an index's map while a call is in progress. `OpenChunkReader(index)` returns an independent disposable McapBufferReader that parses lazily and remains valid after snapshot disposal, sharing immutable native input and summary without another file copy. `OpenChunkMessages` replaces the default cursor only after successful initialization; each `ReadChunkMessages` enumeration owns an independent cursor. Random operations do not move these cursors. Buffer overloads allocate no managed memory; metadata/attachment buffers contain record bodies, while message indexes use 18-byte little-endian rows (u16 channel ID, u64 log time, u64 offset). Insufficient buffers remain untouched. `OpenSummaryRecords()` provides a buffer cursor for all summary fields and declarations.

`McapSansIoReader.CreateLinear/CreateSummary` and a completed summary reader's `CreateIndexed` expose value-type events. `SupplyInput` supplies bytes, `NotifySeeked` confirms seeks, and `InsertChunkData` inserts indexed compressed data. Indexed readers also expose `SetRecordLengthLimit`. Payloads copy into caller spans; no native pointer escapes. `GetSummary` returns an owned snapshot; `OpenSummaryRecords` returns a reusable-buffer cursor. Custom decompressor interfaces are not exposed because upstream provides no registration hook. Built-in Lz4/Zstd decompression is handled by the official Rust library.

## Asynchronous records and errors

`McapAsyncReader.ReadNextRecordAsync(Memory<byte>, CancellationToken)` returns `ValueTask<McapRecordReadResult>`. .NET asynchronous Stream I/O drives the official linear parser, matching the optional Tokio capability without hosting Tokio. It retains pending-record retries. Each session permits one operation at a time; consume each ValueTask exactly once before another operation, ownership transfer or disposal. Cancellation and I/O/parser errors terminate the session. Stream exclusion, `leaveOpen` and `IntoInner` follow the synchronous ownership rules.

The reusable completion source and cached continuation provide a warmed 0 B managed-allocation path even when I/O suspends. The gate measures both calling and dedicated I/O threads, including a direct-await loop. Completion may resume the consumer inline on the I/O thread; the library does not force a ThreadPool dispatch. Arbitrary Streams, consumer await machinery, initialization, errors, buffer growth and owned results are excluded. Native allocations are not constrained by this guarantee.

`McapException.Kind` identifies every upstream `McapError` variant; `Details` retains its structured fields. Wrapper-only failures use `Binding`. Original Stream exceptions remain preserved.

### Recoverable writer errors

`McapWriterOptions.RecoverableErrors` is a fixed flags policy captured at construction. By default it enables all five audited pre-mutation rejections: explicit schema registration with ID zero (`InvalidSchemaIdOnRegistration`), explicit schema conflicts (`ConflictingSchemaOnRegistration`), either channel registration referencing an unknown schema (`UnknownSchemaOnChannelRegistration`), explicit channel conflicts (`ConflictingChannelOnRegistration`), and header/payload writes referencing an unknown channel (`UnknownChannelOnMessageWrite`). Prepared schema/channel registration follows the same policy. Choose any subset; unknown bits are rejected before file creation or Stream ownership acquisition.

A rejected call still throws `McapException`. `CanContinueWriting` is true only when this writer was usable after that rejection; correct the input before retrying. It is not a guarantee about subsequent operations or concurrent callers. Full-message writes with automatic declarations, attachment length errors, ID exhaustion, I/O, callbacks, compression failures and panics remain terminal. The library does not retry automatically. Managed argument/state checks retain their existing behavior.

To make every native writer failure terminal, configure:

```csharp
var options = new McapWriterOptions { RecoverableErrors = McapRecoverableWriterErrors.None };
```

Successful message writes retain the zero-managed-allocation contract; error handling is outside that contract.


## Native memory policy

For large recordings, prefer incremental sessions and caller buffers. BufferReader and existing Snapshot constructors still copy input. `McapIndexSnapshot.OpenMapped(path, options)` instead maps a file read-only without a full owned copy. Keep the file unchanged until the snapshot and every child cursor are disposed. Windows retains read-only sharing restrictions throughout this lifetime; Linux cannot prevent concurrent truncation. Child cursors share input and survive snapshot disposal.

Configure `McapReaderOptions.Memory` for sessions and asynchronous/linear Sans-I/O readers, `McapSummaryReaderOptions.Memory` for summary readers, or `McapQuery.Memory` for queries. Session reader Memory, when non-null, takes precedence over query Memory as a whole. Sans-I/O indexed children inherit the summary policy unless query Memory is supplied. BufferReader adds `(data, mode, ignoreEndMagic, options)`; copied snapshots accept `(data, options)` and sessions expose `OpenIndexSnapshot(options)`. The parameterless snapshot method inherits the session policy. Snapshot child cursors inherit its policy; writer summary cursors use defaults. Policies are captured at construction.

| `McapMemoryOptions` property | Default | Resource |
| --- | --- | --- |
| `MaxOwnedInputBytes` | null / unlimited | Full owned input capacity; not mapped length |
| `MaxPendingBufferBytes` | null / unlimited | Retry or summary-encoding capacity |
| `MaxBufferedSortBytes` | null / unlimited | Fallback payload blocks, descriptors and block-container capacities |
| `MaxRetainedBufferBytes` | 8 MiB | Per-buffer retained capacity after successful delivery, including indexed Stream I/O scratch |

Zero is valid. Growth checks use capacity, not used length. Pending data remains available until delivery; oversized buffers are released after delivery. BufferReader retains the complete message body (including its 22-byte header) to support record/message retry interchange; message sessions retain payload only. Adequate caller buffers avoid pending copies. Raw record delivery preserves the validated original body, including trailing extension bytes accepted by upstream; owned models retain official parsed-field semantics. Summary cursors encode only the requested record.

These are resource-specific budgets, not a native/process total limit. Upstream parser/compressor state, declarations, summaries, random-index helper allocations, managed results and mapped resident pages are excluded. Indexed Stream scratch is measured and subject to retention, but not the pending budget; parser record-length limits constrain upstream record/chunk sizes. Sorting uses payload blocks and sorts descriptors, releasing each block after its final message. `AllowBufferedSort=false` rejects fallback before collection.

Budget errors use `McapException.Kind=Binding` and `Details.resource`, `limit`, `requested` (bytes). Advancement failure terminates the reader. Snapshot construction failures restore source position without consuming pending messages; underlying Stream failures may prevent restoration. There is no automatic retry or disk spill.

`GetMemoryStatistics()` on sessions, buffer readers, snapshots, Sans-I/O and async readers returns an allocation-free value type. It reports current/peak controlled capacity, allocation/expansion count, instrumented data-path copy bytes and mapped length. Capacities cover owned input, delivery buffers, indexed Stream scratch and fallback storage. Copy counters cover input copying, parser feeding, pending/arena storage and caller-buffer delivery, excluding cold description serialization and upstream internal copies. They are not allocator-wide or working-set measurements. Snapshot statistics include its default cursor, not independent child cursors. Shared input is included once per view: do not sum related views. Async statistics require consumption of the outstanding operation. No GC memory-pressure estimate is registered.
