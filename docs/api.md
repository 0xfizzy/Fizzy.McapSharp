# API, lifetime, and data ownership

English | [简体中文](api.zh-CN.md)

`Fizzy.McapSharp` provides MCAP file, Stream, buffer and Sans-I/O operations on .NET 8: Windows x64 and glibc Linux x64/ARM64, built against Ubuntu 22.04. macOS, musl and 32-bit processes are unsupported. Applications define payload encodings and nanosecond clock semantics. Cancellable asynchronous record reading and time sorting are available; payload decoding remains application-defined. See the [official API coverage map](coverage.md).

## Read/write capabilities

Find the capability first, then choose input/output, delivery ownership and access pattern. These are library capabilities; their relationship to official Rust APIs is documented in the [coverage map](coverage.md).

| Capability | Writing paths | Reading paths |
| --- | --- | --- |
| Messages | Header + span, complete message, prepared channel, batch and lease forwarding | Owned enumeration, caller buffer, borrowed callback, batch lease |
| Schemas and channels | Explicit registration or automatic declarations with complete messages | Encountered declaration lookup, immutable descriptions, owned enumeration |
| Metadata | `WriteMetadata`, prepared operation | Enumeration or indexed `ReadMetadata` |
| Attachments | Whole payload, prepared operation, segmented attachment | Enumeration or indexed `ReadAttachment` |
| Raw records | `WritePrivateRecord` for private opcodes | Top-level/expanded records, caller buffer, owned records and record views |
| Summary and indexes | Writer options control generation; `Complete` then summary access | Summary snapshot/cursor, indexed queries, chunk cursors and random seek |

Reading arbitrary records does not imply a generic arbitrary-record writer. Writing uses a file or Stream; reading also offers copied/mapped buffers and direct Sans-I/O. See [writing details](#write-a-recording), [owned/raw reading](#owned-records-summaries-and-raw-records) and [reader adapters](#direct-reader-adapters).

## Choose a writing path

All payload inputs are consumed synchronously. Choose based on how declarations and payload storage are already represented; batching reduces call overhead but is not atomic.

| Path | Declarations and preparation | Suitable use |
| --- | --- | --- |
| `WriteMessage(in header, span)` | Register the channel first; warmed path allocates 0 B managed | Repeated messages on known channels, reusable payload buffers |
| `WriteMessage(McapMessage)` | Observes current descriptions and invokes automatic declarations; convenience work may allocate | Writing existing owned message objects |
| Prepared-channel `WriteMessage` | Prepare an immutable description once; automatic declarations at writing; warmed writes allocate 0 B managed | Repeated full-message writes without repeated description serialization |
| `WriteBatch(headers, storage, ranges)` | Register channels first; caller supplies contiguous payload storage and ranges | Payloads already share one storage region; avoid repacking solely to batch |
| `WriteBatch(lease[, headers])` | Register destination channels; optionally replace headers; shares retained payloads across owners | Forwarding a read batch without concatenating payloads |

Preparation may allocate. `McapPreparedOperation` similarly reuses schema/channel/metadata/attachment control descriptions. Native writer, codec and Stream costs still apply. Detailed contracts: [prepared writes](#complete-messages-and-prepared-writes), [batch forwarding](#borrowing-batches-and-leases).

## Choose message ownership

Choose delivery ownership before tuning allocations. A message session applies its query and ordering independently of the delivery method; the methods below expose the selected messages with different result forms and lifetimes. Availability depends on the reader type.

| Need | API | Managed allocation and payload delivery | Lifetime |
| --- | --- | --- | --- |
| Independent, mutable results | `ReadMessages()` | Allocates result objects and final payload arrays; copies mutable schema bytes and metadata for each result | Independent of the reader |
| Reuse caller storage | `ReadNext(buffer, ...)`, `ReadBatch(...)` | Warmed library path allocates 0 B managed; copies payload into caller buffers | Caller owns buffers and decides when to overwrite them |
| Process synchronously | `ReadNext(visitor)`, `VisitMessages(...)` | Warmed library path allocates 0 B managed; no final payload delivery copy | Span expires when the callback returns; no reader reentry |
| Retain across operations or forward | `ReadBatchLease(...)` | Allocates lease control objects; shares payload backing without batch repacking | Keep the lease alive throughout access; never dispose concurrently |

`ReadMessages()` copies to provide independent mutable results, not because message reading requires owned arrays. Buffer and borrowed paths return headers; retrieve declarations separately when needed. Shared immutable declarations are available through `GetChannelDescription`. Choosing a delivery method does not remove parser input, decompression, query sorting or index costs. See [allocation guarantees](#validation-recovery-and-allocation-guarantees).

Long-lived leases and individual random seeks are valid uses. The [retention guidance](#choosing-storage-for-retained-results) and [access patterns](#complete-chunk-random-access) describe performance choices; the lifetime and concurrency rules are correctness requirements.

## Choose an access pattern

Access selects where and in what order to read; delivery ownership selects how results are handed to the caller. They are separate choices, not competing API families. Available combinations depend on the reader.

| Need | Path | Main tradeoff |
| --- | --- | --- |
| Process a recording in file order | Sequential message/record session | Incremental input; no global result collection for sorting |
| Filter by topic/time in time order | Query with usable indexes | May buffer overlapping chunks; fallback can collect all matches when indexes are insufficient |
| Require indexes, reject scan-and-sort | `OpenIndexedMessages` or disable buffered fallback | Fails when required query support is unavailable |
| Read many messages in one chunk | One `OpenChunkReader` cursor | Reuses traversal within that cursor |
| Fetch a known set of discrete messages | `SeekMessages` | Groups chunk loads within one call; returns a shared batch |
| Revisit chunks across calls | Reuse a snapshot with a cache allowance | Trades retained storage for fewer repeated loads/decompressions |

See [query selection](#choosing-a-query-path), [random access](#complete-chunk-random-access) and [local allowances](#binding-performance-and-local-limits). The following sections define the operations and their lifetime, failure and validation contracts.

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
| `Complete()` | Finishes MCAP and flushes output buffers without requesting file persistence. Repeated successful calls are no-ops; the output remains owned until disposal or Stream transfer. |
| `FlushToDisk()` | After successful Complete, requests file persistence for path outputs or directly supplied FileStream; other Streams throw NotSupportedException. |
| `GetSummary()` | Copies the upstream finish result after successful Complete; may allocate. |
| `Dispose()` | Releases resources without implicit Complete. |

Writer calls are serialized. Applications determine cross-thread business ordering. Native failures outside the configured safe-rejection whitelist and all Stream callback failures make the writer terminal: dispose it and begin a new recording. Argument/state checks before native operations do not by themselves fail a writer. Input buffers can be reused immediately after return.

### Completion and file persistence

Complete the format, then optionally request operating-system file synchronization:

```csharp
using var writer = new McapWriter(path);
// Register channels and write messages.
writer.Complete();
writer.FlushToDisk(); // Optional; omit when only format completion is required.
```

A caller-owned file uses the same two actions:

```csharp
using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write);
using var writer = new McapWriter(stream, leaveOpen: true);
// Register channels and write messages.
writer.Complete();
writer.FlushToDisk();
```

`Complete()` finishes Chunk, Summary, Footer and end magic, then performs ordinary output flushing. `Flush()` during writing does not complete the format. Neither requests file persistence. `FlushToDisk()` calls Rust `File::sync_all()` for path outputs or `FileStream.Flush(true)` for a directly supplied FileStream. Wrapped FileStreams and other Streams are not unwrapped.

Call `FlushToDisk()` only after successful completion and before disposal. Every valid invocation performs synchronization again. Calling before completion throws InvalidOperationException; after disposal it throws ObjectDisposedException. Unsupported Streams throw NotSupportedException without failing the writer. Actual synchronization failures terminate the writer: completion, synchronization and summary access then fail; disposal and Stream ownership transfer remain available. Stream exceptions retain their original type and instance. Previously opened independent summary cursors remain usable.

The path file handle stays open after completion until disposal. `leaveOpen` and `IntoInner()` control Stream ownership as usual. Neither disposal nor ownership transfer implicitly completes or synchronizes the recording. Successful synchronization means the operating-system request succeeded; it is not an unconditional guarantee about hardware, filesystem or directory-entry persistence.

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

Nullable flags use upstream behavior when null. `EmitSummaryRecords` applies first; explicitly supplied individual switches override it. `DisableSeeking` defaults to false for seekable output and true for non-seekable output; explicit false on non-seekable output fails. To omit summary records, disable statistics, chunk/attachment/metadata indexes and repeated declarations. Disable summary offsets separately. No chunks means no chunk compression or chunk message indexes, regardless of their requested settings.

### Long recordings in one file

A single file can retain compression and complete indexes without retaining all message payloads. Writer memory includes active chunk/codec storage, declarations and file-level indexes. The upstream writer accumulates a ChunkIndex for every finished chunk even when `EmitChunkIndexes=false`; that option controls emission, not accumulation. `EmitMessageIndexes=false` omits per-chunk message indexes, while `EmitAttachmentIndexes=false` and `EmitMetadataIndexes=false` stop retaining their respective indexes. Disabling indexes reduces indexed-query capabilities. Reuse declarations instead of continually adding distinct schemas/channels.

Choose `ChunkSize` using recording duration, throughput and random-read latency. Larger chunks reduce chunk-index counts but can increase active storage and the cost of reading/decompressing a chunk. The size is a target, not a memory ceiling. Avoid unnecessary `Flush()` calls: they finish the active chunk and can increase index counts. Use file or draining Stream output and bound the caller's pending-write queue; a growing MemoryStream retains the recording itself. No automatic segmentation or index spill is performed.

`Complete()` releases the upstream writer after finishing and retains one shared native summary plus the output. It does not build a persistent JSON summary. `GetSummary()` encodes the response on demand and returns an independent managed result; requesting the whole summary still requires response and result memory proportional to its contents. `OpenSummaryRecords()` shares the native summary without whole-summary JSON encoding. Independent cursors can retain it after writer disposal. Upstream finish-time summary cloning remains a transient cost. Dispose the writer when summary/persistence/Stream-transfer operations are finished; single-file recording still has file-level index growth, not constant memory.

### Complete messages and prepared writes

`WriteMessage(McapMessage)` invokes upstream `Writer::write`, including automatic declarations using supplied IDs. The prepared overload takes `McapPreparedChannel`, a matching header and payload span, avoiding repeated managed serialization. Preparation snapshots schema bytes and metadata without registering declarations. Prepared descriptors are immutable; convenience calls observe current inputs.

`McapPreparedOperation` provides reusable schema, channel, metadata and attachment descriptors. `WritePrepared` executes without managed allocation and returns registration IDs where applicable; payloads remain separate spans. Attachment continuation, private records and Flush also provide allocation-free paths. `Complete()` explicitly finishes the format; `GetSummary()` returns an owned summary and `OpenSummaryRecords()` a buffer cursor. `IntoInner()` releases native state and transfers Stream ownership without implicit completion.


### Recoverable writer errors

`McapWriterOptions.RecoverableErrors` is a fixed flags policy captured at construction. By default it enables all five audited pre-mutation rejections: explicit schema registration with ID zero (`InvalidSchemaIdOnRegistration`), explicit schema conflicts (`ConflictingSchemaOnRegistration`), either channel registration referencing an unknown schema (`UnknownSchemaOnChannelRegistration`), explicit channel conflicts (`ConflictingChannelOnRegistration`), and header/payload writes referencing an unknown channel (`UnknownChannelOnMessageWrite`). Prepared schema/channel registration follows the same policy. Choose any subset; unknown bits are rejected before file creation or Stream ownership acquisition.

A rejected call still throws `McapException`. `CanContinueWriting` is true only when this writer was usable after that rejection; correct the input before retrying. It is not a guarantee about subsequent operations or concurrent callers. Full-message writes with automatic declarations, attachment length errors, ID exhaustion, I/O, callbacks, compression failures and panics remain terminal. The library does not retry automatically. Managed argument/state checks retain their existing behavior.

To make every native writer failure terminal, configure:

```csharp
var options = new McapWriterOptions { RecoverableErrors = McapRecoverableWriterErrors.None };
```

Successful message writes retain the zero-managed-allocation contract; error handling is outside that contract.



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

`BufferTooSmall` returns the required payload length and message header, leaves the destination untouched and keeps the same record pending. Successful empty messages return `Message` with length zero. EOF is repeatable. Errors terminate message/record advancement. Caller-buffer methods expose no native addresses; separate borrowed methods follow the lifecycle contract below.

Queries accept either one exact `Topic` or a `Topics` collection and apply `[StartTime, EndTime)` to LogTime. Null boundaries are unbounded; reversed boundaries throw, equal boundaries select nothing. Queries default to `LogTime` order, with `ReverseLogTime` and `File` available. Equal-time messages follow file order, reversed for reverse order. Opening messages without a query uses sequential file order, matching the official message stream.

Seekable time-ordered queries use the official `IndexedReader` when summary declarations and chunk coverage suffice and there are no top-level messages. Non-default linear parser flags force high-level queries to scan; explicit indexed reading rejects these unsupported flags. Direct Sans-I/O indexed readers never use scan-and-sort fallback.

### Choosing a query path

| Requested behavior | Execution | Cost |
| --- | --- | --- |
| No query, or File order | Incremental sequential scan | No collection of all matching messages for sorting |
| Time order with sufficient indexes and supported options | Official IndexedReader | May buffer overlapping Chunks |
| Time order without sufficient indexes | Wrapper scan-and-sort fallback | Collects matching messages in native memory before returning the session |

The fallback is a high-level binding capability, not sorting provided by official MessageStream. File order need not follow LogTime: after reading time 30, a later record can still have time 10. Without sufficient indexes or an ordering guarantee, the scan must finish before it can establish global time order. This increases time to the first result and memory use in proportion to selected payloads and sorting data; there is no automatic disk spill.

`AllowBufferedSort` defaults to true, including for non-seekable sources. Set it to false to reject fallback with NotSupportedException before collection while retaining index probing, supported indexed queries and file-order scans. Use `OpenIndexedMessages()` to require usable indexes. The fallback collection allowance is defined under [local limits](#binding-performance-and-local-limits). A successful indexed query is not full-file validation.

`GetChannel(id)` and `GetSchema(id)` copy descriptions already encountered or loaded from a summary. New declarations can appear during reading without creating managed objects in the message loop. IDs alone are returned on the hot path. Description lookup and summary operations allocate.

## Owned records, summaries and raw records

`ReadMessages`, `ReadSchemas`, `ReadChannels`, `ReadMetadata` and `ReadAttachments` provide convenient owned records. File-factory methods open independent sessions. Each file-level classification call scans independently. Classification scans validate and observe every record, but deliver only matching records. Owned message/record enumeration copies directly into its final managed array; schema/attachment classification copies only final binary fields. These paths do not use a managed payload scratch buffer. Parser feeding and decompression still occur. Session convenience methods consume the current cursor; schema/channel/metadata/attachment methods require a record session. Message data and schema/attachment data are managed arrays and remain valid after disposal. Arrays are mutable. Message enumeration caches descriptions privately and copies mutable schema bytes and metadata for each result; modifying one result does not affect subsequent messages.

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


## Direct reader adapters

`McapBufferReader` directly adapts official `LinearReader`, `sans_magic`, `ChunkReader`, `ChunkFlattener`, `RawMessageStream` and `MessageStream`. Select `McapBufferReadMode`; Chunk mode accepts a Chunk record body. `ignoreEndMagic` maps to the slice-reader option. Construction copies input once; advancement drives the official Sans-I/O parser and retains one pending result. GetChannel exposes only encountered declarations, including those without messages. Errors are reported when reached; failed advancement leaves previously encountered descriptions available. Use Stream or Sans-I/O sessions for incremental ingestion. `ReadNextRecord` copies record bodies; message modes also provide header/payload `ReadNext`. In RawMessages mode, `GetChannel` retains every declaration successfully encountered by the upstream iterator, including channels without messages and declarations before a deferred error. Descriptions and owned enumeration allocate.

`McapRecords.Parse` returns typed owned models for every standard record and `McapRecord` for unknown records. `McapRecordView.Parse` validates through official `parse_record` and views caller-owned managed memory. Scalar properties and the `Fields` cursor read UTF-8, maps, arrays and binary fields without creating strings/arrays. `ToOwned` copies explicitly. `ReadFooter` and `GetCompressedDataOffset` directly call the upstream helpers.

`OpenIndexSnapshot()` copies a seekable source and reads its official summary without moving the sequential cursor. `new McapIndexSnapshot(bytes)` also accepts caller bytes. The snapshot owns memory proportional to the file, independently of its original session. It exposes `SeekMessage`, `OpenChunkReader`/`ReadChunkMessages`, message indexes, indexed metadata/attachments, footer, descriptions and summary. Random operations use the supplied index fields, including lengths and channel-offset maps; they do not replace the index with a summary entry having the same offset. Metadata and attachment reads accept caller-provided indexes even without a summary. Chunk message and message-index reads still require summary declarations. Index fields not used by the corresponding upstream helper do not add validation. Do not mutate an index's map while a call is in progress. `OpenChunkReader(index)` returns an independent disposable McapBufferReader that parses lazily and remains valid after snapshot disposal, sharing immutable native input and summary without another file copy. Each `ReadChunkMessages` enumeration owns an independent cursor. Random operations do not move these cursors. Buffer overloads allocate no managed memory; metadata/attachment buffers contain record bodies, while message indexes use 18-byte little-endian rows (u16 channel ID, u64 log time, u64 offset). Insufficient buffers remain untouched. `OpenSummaryRecords()` provides a buffer cursor for all summary fields and declarations.

### Prepared Chunk indexes

For repeated random access, construct `McapPreparedChunkIndex(index)` once and dispose it when finished. Construction snapshots the index fields and channel-offset map, encodes once and parses once with the official Rust implementation. Do not mutate the map during construction; later mutations do not affect the prepared index. Prepared overloads of `SeekMessage`, `OpenChunkReader`, `ReadChunkMessages`, `ReadMessageIndexes` and `GetCompressedDataOffset` avoid repeated index encoding, temporary native scratch and index parsing. Existing index overloads continue to observe current caller fields.

A prepared index can be shared across snapshots. Every operation still applies the original file-range and summary requirements; preparation does not validate a particular file. Calls and disposal are serialized for that descriptor. Child cursors remain usable after the prepared index and snapshot are disposed. Message-index helper allocations, cache misses and decompression remain possible; preparation does not guarantee zero total native allocation.

`McapSansIoReader.CreateLinear/CreateSummary` and a completed summary reader's `CreateIndexed` expose value-type events. `SupplyInput` supplies bytes, `NotifySeeked` confirms seeks, and `InsertChunkData` inserts indexed compressed data. Indexed readers also expose `SetRecordLengthLimit`. Payloads copy into caller spans; no native pointer escapes. `GetSummary` returns an owned snapshot; `OpenSummaryRecords` returns a reusable-buffer cursor. Custom decompressor interfaces are not exposed because upstream provides no registration hook. Built-in Lz4/Zstd decompression is handled by the official Rust library.

## Asynchronous records and errors

`McapAsyncReader.ReadNextRecordAsync(Memory<byte>, CancellationToken)` returns `ValueTask<McapRecordReadResult>`. .NET asynchronous Stream I/O drives the official linear parser, matching the optional Tokio capability without hosting Tokio. It retains pending-record retries. Each session permits one operation at a time; consume each ValueTask exactly once before another operation, ownership transfer or disposal. Cancellation and I/O/parser errors terminate the session. Stream exclusion, `leaveOpen` and `IntoInner` follow the synchronous ownership rules.

The reusable completion source and cached continuation provide a warmed 0 B managed-allocation path even when I/O suspends. The gate measures both calling and dedicated I/O threads, including a direct-await loop. Completion may resume the consumer inline on the I/O thread; the library does not force a ThreadPool dispatch. Arbitrary Streams, consumer await machinery, initialization, errors, buffer growth and owned results are excluded. Native allocations are not constrained by this guarantee.

`McapException.Kind` identifies every upstream `McapError` variant; `Details` retains its structured fields. Wrapper-only failures use `Binding`. Original Stream exceptions remain preserved. Native error text is bounded: messages retain up to 256 UTF-8 bytes and textual detail values up to 128, ending at character boundaries. Shortened values carry `[truncated]`; object details also set `truncated: true`. Numeric fields remain exact. Native OS errors report `osCode` and a fixed message instead of allocating localized OS text. Parse errors retain the root cause text rather than the parser’s decorated backtrace.

## Borrowing, batches and leases

Owned results remain independent: `McapMessage.Data` is a `byte[]` copy and mutable declarations are defensively copied. `ReadNext(McapMessageVisitor)` and `VisitMessages(visitor, maxMessages)` receive an `in McapMessageHeader` and `ReadOnlySpan<byte>` without final delivery copying. Return false to stop normally after the current message. Spans expire on callback return. Reading, seeking, inspecting state or disposing the originating reader inside its callback is prohibited; writing to another writer is supported. Callback exceptions are rethrown after normal ABI return. `GetChannelDescription` returns an immutable declaration snapshot shared by subsequent lookups.

`WriteBatch(headers, payloadStorage, ranges)` synchronously consumes contiguous shared storage with one lock and one ABI call. Counts, ranges and all Channels are checked before writing. Return value is the completed count; `McapBatchWriteException.CompletedCount` excludes the failing record, which may already be partly written. Batches are not atomic. Existing audited safe-rejection settings apply; I/O, compression and post-advancement failures terminate the writer. Input buffers may be reused after return.

`WriteBatch(batchLease)` writes the lease's original headers and payloads. `WriteBatch(batchLease, headers)` replaces the complete header of each message; the header count must equal `batchLease.Count`. Register destination schemas/channels first and supply replacement ChannelIds when remapping; no declarations or mappings are inferred. Both overloads use one writer lock and one ABI call, reference payloads across multiple storage owners without concatenating them, and retain the lease handle for the synchronous call. They do not transfer ownership or alter the lease. Do not dispose the lease concurrently with writing. Null/disposed leases and mismatched header counts are rejected before native writing without failing the writer. Channel preflight, safe rejections and completed-prefix failures follow the same batch contract above. Both overloads have a warmed zero-managed-allocation gate; allocating the input lease is a separate cost.

`ReadBatch(headers, ranges, payloadStorage)` fills caller-owned buffers with whole messages and returns count, used bytes, stop reason and the next required capacity. It preserves the next message when space is insufficient. Borrowed, caller-buffer batch and batch-write paths have warmed Release zero-managed-allocation gates.

`ReadBatchLease` returns `McapMessageBatchLease`, by default up to 256 messages and a soft 4 MiB payload target. The target is checked after adding a whole message, so a batch may exceed it, including when a single message exceeds the target. `GetHeader`, `GetPayload`, `CopyTo` and `RetainMessage(index)` access or retain messages without per-message payload arrays. Batches may reference multiple chunks without repacking. Leases survive reader disposal; Dispose is idempotent, with private SafeHandle finalization fallback. Dispose retained message leases separately.

Keep a lease in a using scope throughout span use. Access methods reject disposed owners, but an existing span cannot be revoked. Keep the owner alive and never overlap span access with concurrent disposal. Cross-thread transfer is supported with caller synchronization. Mapped files must remain unchanged until every reader, child cursor and lease referencing them is released.

`ReadBatchLease` returns a batch, or null at EOF. `ReadBatchLeaseAsync` awaits I/O and supports cancellation; consume its ValueTask before another operation or disposal. Cancellation terminates the reader; delivered leases remain valid. Record and lease consumption cannot be mixed on one async reader. Message leases reject EmitChunks. No prefetch queue, spill file or automatic file segmentation is created.

Async lease reading uses a reusable completion source and continuation. Each delivered batch may allocate its lease and SafeHandle objects; I/O suspension does not require another per-operation async state machine. A nonempty batch is returned when the parser next requests input, even below the message/payload target. This avoids waiting for extra I/O merely to fill a batch. Stream implementations may have their own allocations. Input completion is consumed before cancellation can release parser resources.

## Binding performance and local limits

Performance contracts concern binding-added allocations and payload copies, not allocations inside the official parser, writer or codecs. Borrowed callbacks and leases reference stable storage; caller-buffer reads copy to the caller, and convenience APIs return independent owned results. Copied constructors own an input copy; `OpenMapped` shares a mapping and Stream supports incremental input. Setup, descriptors, lease control objects and errors may allocate.

Synchronous and asynchronous input reserve the parser's current complete requirement independently of the I/O transfer size, avoiding repeated expansion during short reads. This is not whole-source prebuffering or a total-memory bound. A retained slice can keep a complete chunk or mapping alive; limit outstanding work according to storage size as well as batch count. Cache allowances and batch payload targets do not account for every externally retained byte.

| Control | Counted quantity and default | Boundary behavior and exclusions |
| --- | --- | --- |
| `McapQuery.MaxBufferedSortBytes` | Sum of selected messages' logical payload lengths plus allocated descriptor-array capacity in bytes; null means no collection allowance | Exceeding the allowance fails fallback construction/reading, without spilling or partial sorted results. Each selected range counts its length even if backing is shared. Excludes backing amplification, temporary compaction overlap, parser/codec memory and RSS; does not apply to indexed-reader chunk buffering. |
| `MaxRandomAccessCacheBytes` on reader/snapshot options | Local LRU charge for retained chunk storage, descriptors, keys and index bytes; 0 disables retention | Oversized entries load without cache retention. Eviction releases cache references, not caller leases. Excludes allocator overhead, parsing temporaries, snapshot input and externally retained owners. Uncompressed mapped chunks are charged their logical decompressed size. |
| `ReadBatchLease` count and payload target | Default maximum 256 messages; soft target 4 MiB of logical payload | Checks the byte target after each whole message, so it can overshoot. Does not limit backing capacity or all outstanding batches. Async batches may return earlier at an input request. |

Reader cache options apply to snapshots it creates. `AllowBufferedSort=false` rejects fallback collection entirely. Record-length checks remain upstream parser options. None of these controls is a process-wide memory limit.

### Choosing storage for retained results

Use borrowed callbacks for synchronous processing and leases for short asynchronous pipelines or forwarding. For a few messages kept long term, copy only the needed payloads into independent arrays and release the lease:

```csharp
byte[] payload;
using (var batch = session.ReadBatchLease(1))
{
    if (batch is null) return;
    payload = new byte[batch.GetPayload(0).Length];
    batch.CopyTo(0, payload);
}
// payload now has independent ownership.
```

A lease retains backing ownership, not just its visible payload range. `RetainMessage` shares that ownership; it does not trim the allocation. A small message can retain a full decompressed chunk, a complete copied input or a file mapping. A batch may retain several owners, and several batches may share one owner.

Disposal releases that owner's reference; storage can be reclaimed only after all references are released. Other leases, parsers, snapshots, child cursors or caches may still retain it. Copying one slice therefore does not guarantee immediate reclamation. A snapshot itself retains its input until it and every dependent owner release it. Mapping size is address space, not a measurement of resident memory.

For predictable pipeline retention, bound outstanding work using distinct backing sizes and retention duration as well as batch count. Long-term sharing remains valid when retaining the backing is acceptable; copying sparse results is a performance choice, not a lifetime requirement.

Buffered sort chooses storage before publishing results. It groups selected messages by consecutive owned backing, retaining dense groups and compacting sparse groups at an owner change or end of scan. The current internal policy copies when capacity is at least four times selected payload bytes and saves at least 256 KiB of group backing capacity. Compact segments target 256 KiB at message boundaries; larger messages get their own segment. Empty payloads retain no backing. External storage, including mappings, stays shared. These thresholds are implementation choices, not public tuning options or performance guarantees.

Compaction preserves message order, retries and lease lifetimes. It copies selected payloads once and temporarily holds old and new storage together. Other owners can delay actual reclamation. Sorting still collects results before delivery; its logical allowance does not cover transient copies, retained backing amplification, parser/codec memory or process RSS.

### Complete-chunk random access

Cache keys use complete caller-supplied index semantics; hits do not decompress again. Uncompressed mapped payloads reference mapping ranges; compressed payloads share decompressed storage. Insufficient-buffer retries retain shared slices.

`SeekMessages(ReadOnlySpan<McapSeekRequest>)` groups requests by chunk within that call, loads each different chunk once, and returns a batch in request order including duplicates. Failure delivers no partial batch. `SeekMessage(preparedIndex, entry, visitor)` provides borrowed delivery; buffer and owned-result APIs copy at final delivery. `GetCacheStatistics` reports hits and chunk loads.

Choose the reuse scope that matches the access pattern:

| Access pattern | Method | Reuse scope |
| --- | --- | --- |
| One isolated message | `SeekMessage` | Loads the complete target chunk on a cache miss; leaving caching disabled avoids cache retention |
| Many messages in one chunk | `OpenChunkReader` | Traverse one cursor once; reopening a cursor starts a new traversal |
| Known set of discrete requests | `SeekMessages` | Groups loads within one call, even with caching disabled; another call is a separate batch |
| Repeated visits across calls | Reuse a snapshot with `MaxRandomAccessCacheBytes` | Retains recent chunks until eviction or snapshot disposal |

The working set is the chunks revisited between cache reuses. Size the allowance for their decompressed storage and descriptors, not compressed file bytes. A working set larger than the allowance can cause eviction and repeated decompression; this remains a valid access pattern. Inspect `GetCacheStatistics()` hits and chunk loads to assess reuse. Prepared indexes avoid repeated descriptor encoding/parsing, not cache-miss decompression. Copying a returned payload does not cache its source chunk.

Random reads validate the complete target chunk and may report tail corruption earlier than prefix reads. This is not full-file validation. See [local patches](patches.md) for patch boundaries and evidence.
