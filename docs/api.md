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

Nullable flags use upstream behavior when null. `EmitSummaryRecords` applies first; explicitly supplied individual switches override it. `DisableSeeking` defaults to false for seekable output and true for non-seekable output; explicit false on non-seekable output fails. To omit summary records, disable statistics, chunk/attachment/metadata indexes and repeated declarations. Disable summary offsets separately. No chunks means no chunk compression or chunk message indexes, regardless of their requested settings. The Memory option selects a finite shared storage budget, including codec heap workspaces; see the accounting boundary below for unfinished paths and resources outside the controlled heap.

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

`AllowBufferedSort` defaults to true, including for non-seekable sources. Set it to false to reject fallback with NotSupportedException before collection while retaining index probing, supported indexed queries and file-order scans. Use `OpenIndexedMessages()` to require usable indexes. `McapQuery.Memory.MaxBufferedSortBytes` limits the fallback's controlled allocations; it does not cap all native memory or the official indexed reader's overlapping-Chunk buffers. A successful indexed query is not full-file validation.

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


## Complete messages and prepared writes

`WriteMessage(McapMessage)` uses the shared upstream `Writer::write` logic through borrowed declaration fields, including automatic declarations using the supplied IDs. Its zero-allocation counterpart takes `McapPreparedChannel`, a matching message header and a payload span. Preparation snapshots schema bytes and metadata but does not register declarations. `McapPreparedChannel(channel, budget)` uses a supplied resource domain; the original constructor uses an independent default domain. The charged JSON field storage, schema bytes and private root stay in that domain across writer calls and are released on disposal. The convenience overload observes current mutable inputs on every call; prepared descriptors remain immutable.

`McapPreparedOperation` provides reusable schema, channel, metadata and attachment descriptors. Budget overloads accept a shared `McapMemoryBudget`; Schema and Channel take it as the first argument, while Metadata and attachment factories take it last. Existing signatures create an independent default domain. Prepared storage retains its original domain when reused by different writers. Descriptor roots, schema payloads and parsed JSON control storage have exact accounting. JSON strings use a charged arena, and object fields use paged balanced indexes. Ordinary writer control calls charge transient JSON storage to the writer domain and release it on return; an input-block or total-budget rejection terminates the writer. Metadata and Channel writes traverse these fields directly without a temporary map. Nested summary materialization remains open items in [the allocation inventory](memory-accounting.md). `WritePrepared` executes them without managed allocation and returns registration IDs where applicable. Payloads stay separate spans. Attachment continuation, private records and Flush also have zero-allocation paths. `Complete()` explicitly finishes writing; `GetSummary()` then returns an owned summary. `Complete()` plus `OpenSummaryRecords()` supplies a reusable-buffer summary cursor. `IntoInner()` releases native state and transfers Stream ownership without implicitly completing the file.

## Direct reader adapters

`McapBufferReader` directly adapts official `LinearReader`, `sans_magic`, `ChunkReader`, `ChunkFlattener`, `RawMessageStream` and `MessageStream`. Select `McapBufferReadMode`; Chunk mode accepts a Chunk record body. `ignoreEndMagic` maps to the slice-reader option. Construction copies input once; advancement drives the official Sans-I/O parser and retains one pending result. GetChannel exposes only encountered declarations, including those without messages. Errors are reported when reached; failed advancement leaves previously encountered descriptions available. Use Stream or Sans-I/O sessions for incremental ingestion. `ReadNextRecord` copies record bodies; message modes also provide header/payload `ReadNext`. In RawMessages mode, `GetChannel` retains every declaration successfully encountered by the upstream iterator, including channels without messages and declarations before a deferred error. Descriptions and owned enumeration allocate.

`McapRecords.Parse` returns typed owned models for every standard record and `McapRecord` for unknown records. `McapRecordView.Parse` validates through official `parse_record` and views caller-owned managed memory. Scalar properties and the `Fields` cursor read UTF-8, maps, arrays and binary fields without creating strings/arrays. `ToOwned` copies explicitly. `ReadFooter` and `GetCompressedDataOffset` directly call the upstream helpers.

`OpenIndexSnapshot()` copies a seekable source and reads its official summary without moving the sequential cursor. `new McapIndexSnapshot(bytes)` also accepts caller bytes. The snapshot owns memory proportional to the file, independently of its original session. It exposes `SeekMessage`, `OpenChunkReader`/`ReadChunkMessages`, message indexes, indexed metadata/attachments, footer, descriptions and summary. Random operations use the supplied index fields, including lengths and channel-offset maps; they do not replace the index with a summary entry having the same offset. Metadata and attachment reads accept caller-provided indexes even without a summary. Chunk message and message-index reads still require summary declarations. Index fields not used by the corresponding upstream helper do not add validation. Do not mutate an index's map while a call is in progress. `OpenChunkReader(index)` returns an independent disposable McapBufferReader that parses lazily and remains valid after snapshot disposal, sharing immutable native input and summary without another file copy. Each `ReadChunkMessages` enumeration owns an independent cursor. Random operations do not move these cursors. Buffer overloads allocate no managed memory; metadata/attachment buffers contain record bodies, while message indexes use 18-byte little-endian rows (u16 channel ID, u64 log time, u64 offset). Insufficient buffers remain untouched. `OpenSummaryRecords()` provides a buffer cursor for all summary fields and declarations.

### Prepared Chunk indexes

For repeated random access, construct `McapPreparedChunkIndex(index)` once and dispose it when finished. Construction snapshots the index fields and channel-offset map, encodes once and parses once with the official Rust implementation. Do not mutate the map during construction; later mutations do not affect the prepared index. Prepared overloads of `SeekMessage`, `OpenChunkReader`, `ReadChunkMessages`, `ReadMessageIndexes` and `GetCompressedDataOffset` avoid repeated index encoding, temporary native scratch and index parsing. Existing index overloads continue to observe current caller fields.

A prepared index can be shared across snapshots. Every operation still applies the original file-range and summary requirements; preparation does not validate a particular file. Calls and disposal are serialized for that descriptor. Child cursors remain usable after the prepared index and snapshot are disposed. Prepared storage has its own default finite domain or an explicitly supplied shared budget, and is separate from per-snapshot statistics. Message-index helper allocations, cache misses and decompression remain possible; preparation does not guarantee zero total native allocation.

`McapSansIoReader.CreateLinear/CreateSummary` and a completed summary reader's `CreateIndexed` expose value-type events. `SupplyInput` supplies bytes, `NotifySeeked` confirms seeks, and `InsertChunkData` inserts indexed compressed data. Indexed readers also expose `SetRecordLengthLimit`. Payloads copy into caller spans; no native pointer escapes. `GetSummary` returns an owned snapshot; `OpenSummaryRecords` returns a reusable-buffer cursor. Custom decompressor interfaces are not exposed because upstream provides no registration hook. Built-in Lz4/Zstd decompression is handled by the official Rust library.

## Asynchronous records and errors

`McapAsyncReader.ReadNextRecordAsync(Memory<byte>, CancellationToken)` returns `ValueTask<McapRecordReadResult>`. .NET asynchronous Stream I/O drives the official linear parser, matching the optional Tokio capability without hosting Tokio. It retains pending-record retries. Each session permits one operation at a time; consume each ValueTask exactly once before another operation, ownership transfer or disposal. Cancellation and I/O/parser errors terminate the session. Stream exclusion, `leaveOpen` and `IntoInner` follow the synchronous ownership rules.

The reusable completion source and cached continuation provide a warmed 0 B managed-allocation path even when I/O suspends. The gate measures both calling and dedicated I/O threads, including a direct-await loop. Completion may resume the consumer inline on the I/O thread; the library does not force a ThreadPool dispatch. Arbitrary Streams, consumer await machinery, initialization, errors, buffer growth and owned results are excluded. Native allocations are not constrained by this guarantee.

`McapException.Kind` identifies every upstream `McapError` variant; `Details` retains its structured fields. Wrapper-only failures use `Binding`. Original Stream exceptions remain preserved. Native error text is bounded: messages retain up to 256 UTF-8 bytes and textual detail values up to 128, ending at character boundaries. Shortened values carry `[truncated]`; object details also set `truncated: true`. Numeric fields remain exact. Native OS errors report `osCode` and a fixed message instead of allocating localized OS text. Parse errors retain the root cause text rather than the parser’s decorated backtrace.

### Recoverable writer errors

`McapWriterOptions.RecoverableErrors` is a fixed flags policy captured at construction. By default it enables all five audited pre-mutation rejections: explicit schema registration with ID zero (`InvalidSchemaIdOnRegistration`), explicit schema conflicts (`ConflictingSchemaOnRegistration`), either channel registration referencing an unknown schema (`UnknownSchemaOnChannelRegistration`), explicit channel conflicts (`ConflictingChannelOnRegistration`), and header/payload writes referencing an unknown channel (`UnknownChannelOnMessageWrite`). Prepared schema/channel registration follows the same policy. Choose any subset; unknown bits are rejected before file creation or Stream ownership acquisition.

A rejected call still throws `McapException`. `CanContinueWriting` is true only when this writer was usable after that rejection; correct the input before retrying. It is not a guarantee about subsequent operations or concurrent callers. Full-message writes with automatic declarations, attachment length errors, ID exhaustion, I/O, callbacks, compression failures and panics remain terminal. The library does not retry automatically. Managed argument/state checks retain their existing behavior.

To make every native writer failure terminal, configure:

```csharp
var options = new McapWriterOptions { RecoverableErrors = McapRecoverableWriterErrors.None };
```

Successful message writes retain the zero-managed-allocation contract; error handling is outside that contract.


## Borrowing, batches and leases

Owned results remain independent: `McapMessage.Data` is a `byte[]` copy and mutable declarations are defensively copied. `ReadNext(McapMessageVisitor)` and `VisitMessages(visitor, maxMessages)` receive an `in McapMessageHeader` and `ReadOnlySpan<byte>` without final delivery copying. Return false to stop normally after the current message. Spans expire on callback return. Reading, seeking, inspecting state or disposing the originating reader inside its callback is prohibited; writing to another writer is supported. Callback exceptions are rethrown after normal ABI return. `GetChannelDescription` returns an immutable declaration snapshot shared by subsequent lookups.

`WriteBatch(headers, payloadStorage, ranges)` synchronously consumes contiguous shared storage with one lock and one ABI call. Counts, ranges and all Channels are checked before writing. Return value is the completed count; `McapBatchWriteException.CompletedCount` excludes the failing record, which may already be partly written. Batches are not atomic. Existing audited safe-rejection settings apply; I/O, compression and post-advancement budget failures terminate the writer. Input buffers may be reused after return.

`ReadBatch(headers, ranges, payloadStorage)` fills caller-owned buffers with whole messages and returns count, used bytes, stop reason and the next required capacity. It preserves the next message when space is insufficient. Borrowed, caller-buffer batch and batch-write paths have warmed Release zero-managed-allocation gates.

`ReadBatchLease` / `TryReadBatchLease` return `McapMessageBatchLease`, normally up to 256 messages and a soft 4 MiB payload target. A larger message forms its own batch. `GetHeader`, `GetPayload`, `CopyTo` and `RetainMessage(index)` access or retain messages without per-message payload arrays. Batches may reference multiple chunks without repacking. Leases survive reader disposal; Dispose is idempotent, with private SafeHandle finalization fallback. Dispose retained message leases separately.

Keep a lease in a using scope throughout span use. Access methods reject disposed owners, but an existing span cannot be revoked. Keep the owner alive and never overlap span access with concurrent disposal. Cross-thread transfer is supported with caller synchronization. Mapped files must remain unchanged until every reader, child cursor and lease referencing them is released.

TryReadBatchLease returns `BudgetUnavailable` for temporary pressure from outstanding leases; ReadBatchLease throws `McapMemoryBudgetUnavailableException`. Release storage and retry. Permanent size violations fail. `McapAsyncReader.ReadBatchLeaseAsync` waits for released storage without holding the reader lock and supports cancellation. Consume the outstanding ValueTask before another operation or disposal. Cancellation terminates the reader; delivered leases remain valid. Record and lease consumption cannot be mixed on one async reader. Message leases reject EmitChunks. No queue, prefetch, spill file or automatic segmentation is created.

## Native memory policy

`McapMemoryOptions.Budget` accepts a shared `McapMemoryBudget`. Independent readers/writers otherwise create independent domains. Snapshot child cursors inherit the domain. Writer options expose Memory; prepared Chunk indexes have an optional budget constructor argument. Defaults are **256 MiB charged capacity, 64 MiB per storage block, 64 MiB idle pool retention** within the total. Finite defaults are a behavior change. Raise limits explicitly. Copied constructors preserve isolation and reject oversized inputs; use OpenMapped or incremental Stream input for large files. Blocks include framing, so a payload equal to the block ceiling can require a larger block.

A live `McapMemoryBudget` retains charged Scratch storage for both the explicit domain root and its handle, so releasing its readers/writers does not make domain usage zero while the budget itself remains alive. Construction rejects a total budget smaller than its required control storage. The root ledger survives until its last strong or weak owner releases it; last-strong cleanup drains idle storage and breaks weak registry cycles. Domain lookup and notification links share the handle allocation; notification registration and dispatch allocate no native heap.

Additional limits remain: MaxOwnedInputBytes, MaxPendingBufferBytes, MaxScratchBufferBytes and MaxBufferedSortBytes default to null, meaning no additional limit; they never bypass the domain. MaxRetainedBufferBytes remains an 8 MiB per-delivery-buffer limit. MaxRandomAccessCacheBytes defaults to zero (disabled); choose a finite value such as 64 MiB to enable retention.

Input/decompressed blocks, pending/scratch, lease/sort descriptors, prepared Chunk indexes and retained summary/writer metadata participate in capacity accounting before growth. Remaining declaration reservations are conservative. Chunk-index compression names and channel-offset tables use exact charged storage; up to four offsets stay inline, and larger tables use pages. Shared payload storage is charged once even when held by several leases and a cache; idle pool storage stays charged. Sorting references shared payloads and rejects insufficient budgets without changing order or spilling. Long recordings retain writer indexes: raise the finite budget, explicitly disable unneeded indexes, or segment recordings in the consumer.

Zstd and Lz4 encoder/decoder contexts use the pinned codec custom-allocation interfaces. Codec heap requests, allocation headers and output buffers reserve capacity before allocation; rejected callbacks fail the operation without unwinding through C. Codec workspace allocations use the total ceiling, not the payload-block ceiling. CompressionThreads retains its existing behavior. Codec failures after advancement are terminal, not retryable budget pressure. Decoder allocation failures expose resource, limit, requested, current and phase in McapException.Details, plus failureKind (permanent, temporary, system, overflow or codec-panic) and terminal. A temporary capacity classification with terminal=true does not make the failed decoder reusable. `limit` is the applicable ceiling that rejected the request; `domainLimit` is always the resource domain total ceiling. Block and local resource ceilings may be lower. Binding resource checks also report both ceilings and domain occupancy at the check.

These diagnostics use fixed native storage; malformed-frame diagnostics use static codec error names.

`GetDetailedStatistics()` returns a fixed value-type snapshot without managed allocations: nine resource categories (input, decompressed, writer, codec encoder/decoder, index, descriptor, declaration, scratch), charged/current peak, live and unused reservations, allocation activity, instrumented copy/codec traffic, decompression attempts/completions and cache events. CurrentBytes, PeakBytes and IdleBytes belong to the same snapshot. Category current capacities sum to CurrentBytes; category peaks must not be summed. ActiveLeasePayloadBytes and CachedPayloadBytes count each shared heap payload allocation once within each ownership dimension; they overlap with categories and each other and exclude mapped payload pages and descriptor pages.

Public budget statistics are fixed value snapshots. Their constructors and deconstruction signatures are independent of the private ABI; no native layout is promised for these public types. Reading and converting a statistics snapshot does not allocate managed memory.

`ReallocationCount` counts instrumented successful growth replacements. `ImmediatelyReclaimableBytes` reports tracked idle storage and cache-only storage; `MappedLogicalBytes` counts each registered mapping owner once, separately from heap capacity. `Flow.ReclaimedBytes` currently records physical idle-pool releases; direct cache-release attribution remains unfinished. These fields do not establish complete accounting for the open paths in the [allocation inventory](memory-accounting.md).

`GetStatistics()` retains its five-field interface. AllocationCount includes reservation growth; StorageCopyBytes excludes final delivery copies. Detailed AllocationCount counts instrumented committed allocations. Flow counters are cumulative at their instrumented operations; they are not a complete allocator trace. Existing GetMemoryStatistics remains a per-handle view; do not sum related handles. No GC memory-pressure estimate is registered.

Message and chunk indexes, long-lived writer index lists, immutable summary index lists, batch descriptors, cached message ranges and sorting descriptors use bounded pages with a radix directory. Shared summaries retain pages without cloning complete index arrays. Writer message-index records are serialized directly from pages.

**Accounting boundary:** this is not a process working-set or complete allocator ceiling. OS worker stacks/runtime resources and allocator fragmentation remain outside the controlled heap. Remaining uninstrumented controls, cold JSON/record-parsing temporaries and error payloads are unfinished accounting work, not approved exclusions; see the allocation inventory for the current scope. The reclaimable and mapped-length fields cover registered storage, not every still-open allocation path. The native Rust allocator probe includes allocations routed through the new codec callbacks, including process-wide worker allocations; its thread-local counters exclude other threads. Direct foreign allocations outside those callbacks remain outside that probe.

### Complete-chunk random access

The optional byte-limited multi-chunk LRU is scoped to a snapshot source and keyed by complete supplied index semantics. It retains immutable storage, message offsets and full chunk validation. Uncompressed mapped payloads reference mapping ranges; compressed payloads share their final decompression block. Hits do not decompress. Items larger than the cache allowance can load without retention if the domain has capacity. There is no unlimited helper fallback. Eviction drops only cache ownership; active leases stay charged. Domain pressure first releases idle storage, then visits registered caches in access order across readers. Busy cache entries are skipped; callbacks and destruction run without the budget lock. Per-cache limits remain in effect.

`SeekMessages(ReadOnlySpan<McapSeekRequest>)` groups prepared indexes, loads each different chunk once, and returns one batch lease in original request order, including duplicates. Failure returns no half batch. `SeekMessage(preparedIndex, entry, visitor)` provides borrowed delivery. Existing buffer/owned methods copy at final delivery. Insufficient-buffer message retries retain shared slices. Repeated message indexes can be cached within the same allowance. GetCacheStatistics reports hits and chunk loads, including uncompressed loads.

Random reads validate the complete loaded chunk, including its tail, so corruption may be reported earlier than with prefix reads. This is not full-file validation. Complete scans preserve declaration, boundary, CRC, end-marker and recovery behavior; strict prevalidation delivers after chunk verification. Use cursors for sequential replay.
