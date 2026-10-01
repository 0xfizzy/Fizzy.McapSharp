# Official Rust API coverage

English | [简体中文](coverage.zh-CN.md)

For API users choosing a direct Rust-equivalent operation, this map targets the exact `mcap` dependency in `native/Cargo.toml`. The [declaration inventory](api-coverage.json) records 344 public declarations, including methods, fields, error variants and constants, with managed/native mappings and verification references. `python scripts/check_api_coverage.py` compares it against the locked Cargo source; inventory completeness is separate from behavioral testing.

| Official surface | Managed entry | Verification |
| --- | --- | --- |
| `Writer::write` | `WriteMessage(McapMessage)` and prepared-channel overload, both directly invoking upstream write | Automatic declarations, immutable snapshots, allocation gate |
| Other `Writer` methods | Registration, known-channel messages, attachments, metadata, private records, Flush, Complete/GetSummary and IntoInner | Round-trip, ownership, interoperability and allocation tests |
| `WriteOptions` | `McapWriterOptions`; aggregate switch before explicit individual overrides | Native differential default/configuration tests |
| `read::LinearReader`, `ChunkReader`, `ChunkFlattener`, `RawMessageStream`, `MessageStream` | `McapBufferReader` modes; official Sans-I/O with matching slice-reader configuration | Slice-reader, record-model and allocation tests |
| `Summary::read`, `stream_chunk`, `seek_message`, `read_message_indexes` | `McapIndexSnapshot` and its summary/chunk/message/index operations | Random-access and retry tests |
| `read::attachment`, `metadata`, `footer`, `parse_record` | Snapshot indexed reads; `McapRecords` and `McapRecordView` | Record models, footer, CRC and allocation tests |
| `records::*`, opcodes, format constants | Owned record models, caller-memory field views, `McapOpcode`, `McapFormat` | All standard record models; unknown record preservation |
| Sans-I/O linear, summary and indexed readers/options/events | `McapSansIoReader`, reader/query options and value-type events | Input/seek events, sorted multi-topic reads, limits and retries |
| Public decompressor trait | Not exposed: upstream has no custom-decompressor registration hook | Source review only; no managed implementation |
| Optional Tokio linear reader | `McapAsyncReader`, driven by .NET asynchronous I/O and official Sans-I/O | Cancellation, ownership, forced-suspension allocation gate |
| `McapError` / `McapResult` | `McapException.Kind/Details`; return values and exceptions | Exhaustive native variant match and field tests |

Rust lifetimes, `Cow`, `Arc`, iterator traits and builder methods map to owned results, caller-memory views, disposable sessions and .NET option properties. No public Rust layouts or handles cross the boundary. Buffer adapters copy or explicitly map input and parse lazily; index snapshots copy or explicitly map their source and share it with independent lazy Chunk cursors. Sorted fallback collects selected messages unless AllowBufferedSort is false. These ownership choices require native memory proportional to the input/results; streaming sessions remain available.

Defaults follow the corresponding upstream API: Zstd, 1 MiB writer chunks, upstream Library, file order for sequential messages, LogTime for indexed queries, and disabled optional Sans-I/O CRC checks. Direct slice readers retain their own upstream defaults. Path creation protection, terminal failure outside the configurable safe-rejection whitelist, explicit completion and disposal without implicit completion remain deliberate safety differences.

For zero managed allocation, use prepared writes, caller-buffer reads, record views, summary cursors or the reusable asynchronous reader. Owned convenience results allocate. See [API contracts](api.md), [ABI](native.md) and [validation](development.md) before selecting an entry point.

Behavioral CI additionally uses pinned official conformance data: 416 streamed cases, 32 indexed cases and 208 exact-byte writer cases. Unsupported variants follow the pinned official Rust runner rules and are counted separately, not reported as passes. Fixed-seed differential tests compare .NET and Python producers, and deterministic tests cover truncation, I/O failures, retries and resource contracts. Weekly native mutation/Valgrind and large-file checks extend this coverage; they do not mean that all upstream language-specific tests have been ported. See [external suites and deep checks](development.md#external-contract-suites) for commands, limits and failure reports.


Local shared-storage and channel-query declarations are marked `local-extension`; official declarations are marked `upstream`. The fixed official baseline [upstream-api.json](upstream-api.json) comes from unmodified mcap. Local extensions do not count toward official coverage. Inventory checks cannot establish behavioral equivalence.

`BatchTests`, `LeaseTests`, `BatchSeekTests` and the Release allocation gate cover binding batches, borrowing and leases. `scripts/test_upstream.py` checks bounded format scenarios against an independent unmodified upstream process. See [local patches](patches.md) for necessity, alternatives, ownership and validation boundaries.

`LeaseWriteTests` verifies binding-provided lease batch forwarding and replacement headers through upstream known-channel writes, including pointer identity, preflight and completed-prefix failures. `InputReservationTests`, `LeaseStorageTests`, `AsyncLeaseStateTests`, native memory probes and `LeaseGate` cover input reservation, shared retention/eviction, cancellation and suspension costs. These are binding behaviors, not new official Rust APIs.

Separate Complete/FlushToDisk, local caching and sort fallback are binding behavior. Caller-buffer reads perform a final copy; convenience APIs create independent copies. Performance contracts do not constrain upstream internal allocations or copies.
