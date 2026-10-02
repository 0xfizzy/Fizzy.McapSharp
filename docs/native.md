# Native ABI and memory boundaries

English | [简体中文](native.zh-CN.md)

.NET uses Cdecl P/Invoke and SafeHandle to call a private Rust `cdylib` backed by official `mcap` 0.25.0. There is no C++ layer. [lib.rs](../native/src/lib.rs) implements MCAP operations; [io.rs](../native/src/io.rs) implements file/Stream I/O. Managed declarations are in [Native.cs](../src/Native.cs). Windows x64 loads `fizzy_mcap_native.dll`; glibc Linux x64/ARM64 load `libfizzy_mcap_native.so` through the extensionless name `fizzy_mcap_native`.

## ABI contract

`fm_abi_version()` returns 13. Managed constructors reject mismatches, including older native libraries without lease batch writing. This is not a stable third-party ABI. Incompatible changes must update both version checks and all platform assets together.

| Entry | Purpose |
| --- | --- |
| `fm_writer_open` / `fm_writer_free` | Create/release a writer, optionally with Stream callbacks. |
| `fm_writer_message` | Fixed message header plus borrowed input pointer/length, consumed synchronously. |
| `fm_writer_call` | Cold control operations and attachment/private record payloads. |
| `fm_reader_open` / `fm_reader_free` | Create/release one read session. |
| `fm_reader_next` | Copy the next message payload or raw record body into caller memory. |
| `fm_reader_describe` | Copy a known schema/channel description; schema bytes are separate binary data. |
| `fm_reader_summary` / `fm_reader_record_at` | Retrieve a summary or random raw record without consuming the pending sequential record. |
| `fm_validate` | Fully scan a mapped file. |
| `fm_buffer_free` | Release a Rust-owned response allocation. |

Control operations: 1 schema, 2 channel, 4 metadata, 5 attachment, 6 flush, 7 complete, 8 start attachment, 9 attachment bytes, 10 finish attachment, 11 private record, 12 completed writer summary, 13 completed file synchronization. Messages use their dedicated entry.

The private message header is 24 bytes: `u16 channel_id`, `u16 reserved`, `u32 sequence`, `u64 log_time`, `u64 publish_time`, with offsets 0, 2, 4, 8, 16. It is converted to/from upstream records; it is not a Rust record layout. At reader EOF, reserved is 1 for indexed scans and sorted fallback and 0 for sequential scans. Managed code uses that distinction when reporting full validation.

The 40-byte response contains a JSON pointer/usize length, binary pointer/usize length and u64 value. Pointers and nuint are 64 bits on supported targets. Status 0 is success, 1 EOF, 2 insufficient destination, negative error. Read value reports required/copied bytes or scan count at EOF. Insufficient capacity neither writes a partial result nor consumes the pending record.

Writer open options include `recoverableErrors`, an immutable bit mask (1 invalid explicit schema ID, 2 explicit schema conflict, 4 unknown schema during channel registration, 8 explicit channel conflict, 16 unknown channel during header/payload message writing). Missing means 31; unknown bits fail before output creation. Writer status -2 preserves the ordinary structured error response and leaves the writer usable; status -1, including panic, is terminal. Managed code uses this call status only after checking callback exceptions, never the exception type, to set `CanContinueWriting`. Reader errors do not acquire recovery semantics.

## Data and lifetime

Hot messages use only fixed data and caller buffers. No JSON, managed payload arrays, native result allocation or per-message channel description serialization is required at that boundary. The reader can allocate native buffers internally and copies native bytes into managed caller memory. Cold requests/descriptions use length-delimited UTF-8 JSON; binary data is never Base64-encoded.

Cold responses use Rust-owned JSON/binary allocations released with `fm_buffer_free`; `Native.Consume` frees both in finally. Error JSON contains kind, message and details without fixed-length truncation. Error formatting is also protected by a panic boundary. Successful hot calls return no owned response buffers. Input spans are pinned only during synchronous calls. Convenience results own managed copies; callbacks and leases follow the API lifetime contract.

Each read session owns one native reader. Mapped input owns its file; incremental `sans_io::LinearReader` state and pending records contain native-owned buffers, without extending a borrowed iterator's lifetime. Stream reads are incremental, including short reads. Indexed queries use official IndexedReader, which may retain overlapping chunks for time sorting, and do not claim whole-file validation. Sorted sequential fallback collects matching messages in native memory. Missing summary declarations fall back to sequential reading; resolving a partial summary may require a cold full scan.

Mapped files must remain unchanged. Windows denies ordinary competing write/delete opens, while existing writable mappings are outside that protection. Linux does not enforce exclusion. Concurrent truncation can terminate the process; this is outside panic/exception handling. Record bounds are checked against source size for random reads. Native allocation and decompression still require memory proportional to records/chunks; upstream allocation and codec policies remain unchanged.

## Stream callbacks and release

The callback table is 48 bytes: context pointer, Read/Write/Seek/Flush function pointers, then `u32 seekable` and alignment padding. Cdecl callbacks report status, with byte count/position through output pointers. The managed bridge roots its callback context once per session. SafeHandle releases the native handle before unrooting the context and closing an owned stream.

Callbacks run synchronously on the initiating thread. Managed callback exceptions are captured and returned as failure; after native code unwinds normally, the original exception is rethrown. Reentry is rejected. A stream cannot belong to two concurrent sessions. Seek offsets are relative to the captured MCAP origin; non-seekable writers allow only a current-position query and use upstream `disable_seeking(true)` buffering.

Writer operations are serialized. Writer status -2 is a configured, audited pre-mutation rejection; -1 is terminal. Other native errors are terminal. `Complete` calls upstream finish and ordinary output flush, retaining output until release. Operation 13 requires successful completion and a native File output, then calls sync_all. FileStream persistence uses managed Flush(true) under the writer lock and reentry guard; callback layouts are unchanged. Synchronization failures are terminal. Drop uses upstream `into_inner`, preventing implicit completion. Free operations catch destructor panics. No callback pointers remain usable after release.

Writer completion extracts the output immediately after upstream `finish`, releasing the upstream writer's cached summary and declarations. The binding retains only `Arc<Summary>`; operation 12 encodes fields and index entries into the final UTF-8 response on demand, with at most the current record/statistics JSON value as intermediate storage. No complete JSON tree is retained. Summary cursors share the Arc independently of the writer. The upstream finish-time summary clone and file-level index accumulation are unchanged; this is a binding ownership optimization, not an upstream memory bound or ABI change.

## Error boundary and validation

Fallible native exports catch panics and convert them into error responses. Allocation aborts and invalid externally supplied pointers cannot be converted into managed exceptions; callers must pass valid buffers and handles created by this ABI. Rust compile-time assertions and managed tests check supported layout sizes and offsets.

The public [API contract](api.md) distinguishes complete validation, indexed queries and raw records. [Build and allocation acceptance](development.md) runs managed allocation gates separately from format interoperability; zero managed allocation does not imply zero native allocation.


## Extended operation families

`fm_channel_prepare/free` own immutable Channel/Schema snapshots. `fm_writer_full_message` invokes upstream writing with the descriptor and synchronous borrowed payload. `fm_operation_prepare/free` own parsed cold control data; `fm_writer_prepared` reuses descriptors without managed serialization. `fm_writer_private` accepts scalar flags and a span.

`fm_engine_open/next/feed/free` wrap official linear, summary and indexed Sans-I/O state. The 56-byte event layout is: u32 kind, u32 opcode, u64 length, u64 offset, u32 seek origin, u32 reserved and the 24-byte message header. Kinds 0–5 are End, Read, Seek, Record, Message and ReadChunk. Seek offsets preserve signed two's-complement bits for Current/End. Read requests remain pending until supplied; record/message events remain pending on insufficient buffers. `fm_engine_index_control` supports indexed insertion and length-limit changes. `fm_engine_summary` and `fm_summary_records` export owned summaries or native-owned record cursors.

`fm_buffer_reader_*` owns an input copy and an official Sans-I/O parser configured to match each slice reader. Advancement retains one pending record and encountered declarations; no borrowed iterator survives a call. Chunk adapters feed a synthetic record prefix through the public parser because upstream for_chunk is private. `fm_snapshot_*` owns copied or mapped input and an official summary; random message reads retain shared complete-chunk storage through private handles. `fm_snapshot_call` accepts a synchronous pointer/length to a standard MCAP index record body and both message-index scalars (log time and offset). It parses the supplied index rather than looking it up in the summary. The managed bridge uses bounded stack storage or temporary native memory for index encoding, including UTF-8 strings and channel-offset maps. `fm_reader_record_into`, `fm_parse_record`, `fm_footer` and `fm_chunk_offset` provide caller-buffer or scalar utility operations. `fm_snapshot_chunk_reader` adds independent lazy Chunk cursors sharing immutable native input and summary; releasing a snapshot does not invalidate those cursors. Reader-open status 3 means buffered sorting was prohibited, mapped to managed NotSupportedException. These handles are private SafeHandles; cursor reads have no owned response allocation on success.

The asynchronous reader drives the linear engine with .NET ReadAsync, retaining only managed Memory between waits. Its reusable completion source and cached continuation avoid per-operation managed allocations. A resource SafeHandle releases the parser before releasing Stream ownership, including abandoned-reader finalization. Cancellation terminates the session; disposal requires consumption of the outstanding operation.


## Stable storage and batching

`fm_writer_batch` receives 24-byte headers, 8-byte offset/length ranges, shared payload and a separate completed-prefix output. `fm_read_batch`/`fm_visit_messages` use a 40-byte Progress (four u64, two u32). Preflight precedes writes; advancement failures do not roll back.

`fm_writer_lease_batch` receives writer and lease handles, an optional header pointer with usize count, a usize completed-prefix output, and the ordinary Response. Null headers with count zero select the lease's headers; otherwise the count must match the lease's messages. All destination Channels are checked before writing. Payload slices are read directly from the retained batch; no payload or descriptor array is constructed. Managed code holds an explicit SafeHandle reference to the lease across the call. Header layouts and batch error semantics are unchanged.

`fm_reader_owned`, `fm_buffer_reader_owned` and `fm_snapshot_message_owned` use a two-pointer Sink (context, Cdecl callback). Visitor result 1 is normal stop, negative is failure; managed exceptions never unwind across FFI. Every exit clears the sink. Public borrowed spans expire when callbacks return; owned delivery creates independent copies.

`fm_read_lease`, `fm_engine_lease_step` and `fm_lease_get/retain/free` use SharedBytes and ordinary batch descriptors. Storage ownership retains mappings and files after reader disposal; span access must not overlap disposal. `fm_engine_input_buffer/complete` lets Stream.ReadAsync fill parser storage directly through a MemoryManager, without concurrent engine advancement or destruction.

Pending delivery retains shared bytes or a synthesized record body. Buffer readers retain the complete Message body and choose its payload range at delivery, supporting record/message retry switching. Indexed Stream input transfers buffer ownership into the parser, avoiding an extra uncompressed-chunk copy; short reads use read_exact. Cache and sort enforce only the local allowances documented in the API.

Synchronous linear feeding reserves the complete current parser request through try_insert and reads at most 64 KiB into that storage per call. Sequential Stream reads, summary fallback scans and validation use the same helper. Async direct-fill similarly reserves the complete request while exposing only the selected I/O quantum. Async lease delivery uses a reusable explicit state machine and the cached I/O continuation; lease result/control allocations remain permitted. Nonempty batches still return at the next input request.

Fallback sorting uses a binding-owned arena of headers, ordinals and shared ranges. One fixed-size pending group tracks consecutive owned backing identity and selected bytes; it does not build a file-wide owner map. At group closure, sparse selections may be copied once into message-aligned immutable segments before publication. Mappings and other external owners are not compacted. `SharedBytes::shares_backing` is a read-only local ownership query; compaction decisions and allocations remain in the binding. Existing logical sort checks are unchanged, and old/new storage overlap is outside that allowance. Failures terminate construction without publishing a partial session. There is no ABI change.

The pinned mcap crate supplies official format state machines, codecs and default writing behavior. Shared storage, channel lookup for batch preflight, and disposal without implicit completion are [local patches](patches.md). SafeHandle, managed copies, local cache/sort and asynchronous I/O are binding behavior.
