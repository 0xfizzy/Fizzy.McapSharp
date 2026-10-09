# Official Rust API coverage

English | [简体中文](coverage.zh-CN.md)

Use this guide to locate the .NET entry for an official Rust capability and understand its adaptation. The baseline is the exact `mcap` dependency in [Cargo.toml](../native/Cargo.toml). For ownership, allocation and usage choices, start with the [API guide](api.md#choose-message-ownership).

## Reading the map

The tables group capabilities, not individual declarations. One managed entry can cover several Rust constructors, builder methods or iterator operations. **Adapted** means the official capability is exposed through managed types or composed operations; it does not claim identical signatures, ownership or all behavior. **Alternative** means a comparable managed capability uses another official surface. **Not exposed** is an explicit exclusion, not a covered implementation.

| Source | Purpose |
| --- | --- |
| [upstream-api.json](upstream-api.json) | Fixed inventory of declarations from unmodified upstream; do not change it to accept local extensions |
| [api-coverage.json](api-coverage.json) | Symbol-level mappings, capability group, mapping category, origin, implementation/test references and exclusion reasons |
| This guide | Capability navigation and material adaptation differences; detailed usage stays in the API guide |

The reviewed inventory contains 344 official declarations and 18 local extension declarations. These include types, methods, fields, constants and error variants; the counts are not counts of implemented operations or passing tests. Optional Tokio declarations are included even though the binding does not invoke Tokio.

## Writing (`writing`)

Managed entry points are on [McapWriter](../src/McapWriter.cs) and its partial implementations.

| Official Rust API | .NET entry | Mapping | Key difference or constraint |
| --- | --- | --- | --- |
| `WriteOptions`, `Writer::new/with_options` | `McapWriterOptions`, `McapWriter` constructors | Adapted | Builder settings become properties; aggregate summary switch applies before explicit individual overrides |
| `Writer::add_schema/add_schema_with_id`, `add_channel/add_channel_with_id` | `RegisterSchema`, `RegisterChannel` overloads | Adapted | IDs and binary payload remain separate; prepared descriptors are binding additions |
| `Writer::write` | `WriteMessage(McapMessage)`, prepared-channel overload | Adapted | Invokes upstream automatic declarations; preparation snapshots mutable descriptions |
| `Writer::write_to_known_channel` | `WriteMessage(in header, span)` | Adapted | Synchronously consumes borrowed payload; no managed payload copy |
| `Writer::attach`, `start_attachment/put_attachment_bytes/finish_attachment` | `WriteAttachment`, `StartAttachment/WriteAttachmentBytes/FinishAttachment` | Adapted | Segmented attachment requires exact declared length |
| `Writer::write_metadata`, `write_private_record`, `PrivateRecordOptions` | `WriteMetadata`, `WritePrivateRecord` | Adapted | Private-record placement becomes `includeInChunks` |
| `Writer::flush`, `finish`, `into_inner` | `Flush`, `Complete/GetSummary`, `IntoInner` | Adapted | Completion and summary retrieval are separate; disposal does not implicitly complete. `FlushToDisk` is a binding addition |

Defaults follow upstream writer settings, including Zstd, 1 MiB target chunks and the upstream library identifier. Path creation protection and terminal failure outside the configured safe-rejection whitelist are managed contracts. See [writing](api.md#write-a-recording) for the complete rules.

## Sequential reading (`sequential-reading`)

| Official Rust API | .NET entry | Mapping | Key difference or constraint |
| --- | --- | --- | --- |
| `read::LinearReader`, `Options`, `sans_magic` | `McapBufferReader` Linear/SansMagic modes | Adapted | Official Sans-I/O with matching slice-reader configuration; copied or explicitly mapped input |
| `read::ChunkReader` | `McapBufferReader` Chunk mode | Adapted | Takes a Chunk record body and advances lazily |
| `read::ChunkFlattener` | `McapBufferReader` FlattenChunks mode | Adapted | Expands chunks into records |
| `read::RawMessageStream`, `RawMessage`, `get_channel` | RawMessages mode, header/payload reads, `GetChannel` | Adapted | Retains encountered declarations; no borrowed Rust iterator crosses the ABI |
| `read::MessageStream` | Messages mode and owned message enumeration | Adapted | Owned results copy payload and mutable declarations; other delivery forms have different ownership |

[McapBufferReader](../src/McapBufferReader.cs) consolidates these slice-reader interfaces. `McapFileReader.OpenMessages/OpenRecords` additionally provides file/Stream sessions driven by official parsing. Sequential file order, input ownership and delivery ownership are separate choices; see [reading](api.md#read-messages-into-reusable-buffers).

## Summary and random access (`random-access`)

| Official Rust API | .NET entry | Mapping | Key difference or constraint |
| --- | --- | --- | --- |
| `Summary`, `Summary::read` and summary fields | `McapIndexSnapshot`, `GetSummary`, `OpenSummaryRecords` | Adapted | Snapshot owns copied or mapped input; managed summary and record cursor are different representations |
| `Summary::stream_chunk` | `OpenChunkReader`, `ReadChunkMessages` | Adapted | Independent lazy cursor shares input and summary, surviving snapshot disposal |
| `Summary::seek_message` | `SeekMessage` overloads | Adapted | Binding loads/validates the complete chunk; tail corruption can be reported earlier than upstream prefix seek |
| `Summary::read_message_indexes` | `ReadMessageIndexes` | Adapted | Caller-buffer form uses packed 18-byte rows |
| `read::attachment`, `read::metadata` | `ReadAttachment`, `ReadMetadata` | Adapted | Uses caller-supplied indexes and validates source ranges |

See [McapIndexSnapshot](../src/McapIndexSnapshot.cs). Prepared indexes, grouped seeks and cache retention are binding additions, not additional official methods. Their reuse scopes are described under [random access](api.md#complete-chunk-random-access). Successful indexed reading does not establish full-file validation.

## Sans-I/O (`sans-io`)

| Official Rust API | .NET entry | Mapping | Key difference or constraint |
| --- | --- | --- | --- |
| `sans_io::LinearReader`, `LinearReaderOptions`, `LinearReadEvent` | `McapSansIoReader.CreateLinear`, `McapReaderOptions`, `NextEvent/SupplyInput` | Adapted | Value events and caller buffers replace Rust event borrows and writable input slices |
| `sans_io::SummaryReader`, options and events | `CreateSummary`, `McapSummaryReaderOptions`, input/seek notification and summary access | Adapted | Caller drives I/O; completion makes summary available for indexed reading |
| `sans_io::IndexedReader`, options, events and `ReadOrder` | `CreateIndexed`, `McapQuery`, `McapReadOrder`, indexed control operations | Adapted | Direct indexed engine requires indexes and has no scan-and-sort fallback |

See [SansIo.cs](../src/SansIo.cs) and the symbol inventory for individual events and controls. Optional CRC checks default to upstream settings. Shared-event and direct-fill ownership extensions are listed separately below.

## Records, utilities and errors (`records-and-errors`)

| Official Rust API | .NET entry | Mapping | Key difference or constraint |
| --- | --- | --- | --- |
| `Schema`, `Channel`, `Message`, `Attachment`, `Compression` | Corresponding `Mcap*` models and `McapCompression` | Adapted | Managed ownership replaces `Cow`, `Arc` and Rust lifetimes |
| `records::*`, opcodes and format constants | Owned record models, `McapRecordView.Fields`, `McapOpcode`, `McapFormat` | Adapted | Fields and variants may be reached through a view or owned model rather than a separate method |
| `read::parse_record`, `read::footer`, chunk data-offset helper | `McapRecords.Parse`, `McapRecordView.Parse`, `ReadFooter`, `GetCompressedDataOffset` | Adapted | Owned parsing copies; views retain caller-memory lifetime rules |
| `McapError`, `McapResult` | `McapErrorKind`, `McapException.Kind/Details`, return values and exceptions | Adapted | Structured error conversion replaces Rust results; .NET argument/disposal/Stream exceptions retain their own contracts |

## Asynchronous and unexposed surfaces

| Official Rust API | .NET entry | Mapping | Reason or constraint |
| --- | --- | --- | --- |
| Optional `tokio::LinearReader` (`async`) | `McapAsyncReader.ReadNextRecordAsync` | Alternative | .NET asynchronous I/O drives official Sans-I/O; Tokio itself is not invoked. Cancellation and suspension follow managed contracts |
| `sans_io::Decompressor`, `DecompressResult` (`unexposed`) | None | Not exposed | Upstream has no custom-decompressor registration hook; there is no managed implementation of the trait |

Functional alternatives do not imply a port of runtime-specific interfaces. See [asynchronous reader](../src/McapAsyncReader.cs).

Async message leases are a binding capability over the linear engine. `GetChannelDescription` and `GetSchemaDescription` expose immutable declarations encountered in lease mode after the outstanding read has been consumed; this adds no upstream API or patch.

## Local extensions and binding capabilities

Inventory entries with `origin: local-extension` and group `local-extensions` identify patched native declarations. They are excluded from official counts. Binding-only managed capabilities need not introduce a Rust public declaration and are not added to the official baseline.

| Layer | Capability | Contract and evidence |
| --- | --- | --- |
| Local native patch | Shared storage/events, direct-fill ownership, `shares_backing` | Immutable published ranges and retained ownership; pointer, advancement, disposal and eviction checks |
| Local native patch | `Writer::contains_channel`; disposal after output extraction | Batch preflight and disposal without implicit completion; writer/lease tests |
| Managed/native binding | Prepared descriptors, borrowed delivery, batches, leases and lease forwarding | Explicit ownership and completed-prefix failure rules; `BatchTests`, `LeaseTests`, `LeaseWriteTests`, allocation gates |
| Managed/native binding | Cache, grouped seek, fallback sorting and adaptive storage | Local allowances, reuse scopes and retention; `BatchSeekTests`, `SortStorageTests`, native diagnostics |
| Managed/native binding | Async scheduling, input reservation, `Complete`/`FlushToDisk` separation | Managed suspension/lifetime and persistence contracts; async, reservation and writer completion tests |

Patch motivation and permitted scope belong in [patches.md](patches.md); ABI ownership belongs in [native.md](native.md). Public usage and performance guarantees belong in [api.md](api.md).

## Verification and maintenance

Run `python scripts/check_api_coverage.py` after changing the map. It compares symbol sets and kinds with the selected native source, checks official/local origin against the fixed baseline, and validates mapping categories, groups, required reasons and referenced files. It does not verify that every mapping description is semantically correct or that the two implementations behave identically; those require review and behavioral tests.

Behavioral evidence is separate: pinned conformance cases, .NET/Python interoperability, independent unmodified-upstream comparison, lifetime/retry tests and allocation gates address different properties. Tests against the patched source alone are not independent upstream evidence. Binding diagnostics do not establish total-memory bounds or constrain codec allocations. Commands, fixture counts and limitations are maintained in [development.md](development.md#external-contract-suites).

To review one capability, locate its group above, find its exact `rust` symbol in the reviewed inventory, then inspect the mapped source and evidence. Preserve the official baseline when adding local capabilities; record alternatives and exclusions explicitly instead of counting them as direct implementations.
