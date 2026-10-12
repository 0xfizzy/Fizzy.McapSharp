# Compression, chunks and indexes

English | [简体中文](../zh-CN/writer-options.md)

For applications choosing a recording layout after the [writing guide](writing.md). Start with defaults, then change options for a concrete storage or reading requirement. Configuration is fixed when constructing the writer. Exact defaults and option precedence remain in [API contracts](api.md#writer-options) and the [McapWriterOptions reference](xref:Fizzy.McapSharp.McapWriterOptions).

## Start with defaults

The default uses Zstd compression, chunks with a 1 MiB uncompressed size target, upstream-enabled indexes/summary emission and binding-enabled CRC generation. Omitting options and supplying empty options are equivalent. Nullable emission switches keep upstream settings unless their documented group option sets them; null is not generally the same as false.

```csharp
var options = new McapWriterOptions
{
    Compression = McapCompression.Lz4,
    ChunkSize = 1024 * 1024
};
using var writer = new McapWriter(path, options); // path must not exist
// Register declarations and write as in the writing guide.
writer.Complete();
```

This configuration illustrates an explicit choice, not a recommended performance optimum. Measure representative payloads and the intended downstream queries before changing it.

## Choose compression and chunk size

`None`, `Lz4` and `Zstd` select chunk compression. With `UseChunks = false`, chunk compression and chunk/message indexes are unavailable. Compression level is codec-specific; Zstd worker settings control compression workers, not reader concurrency.

Larger chunks can reduce chunk counts and index entries, while increasing active storage and the work to load/decompress a chunk for a sparse read. Smaller chunks trade those costs in the opposite direction. `ChunkSize` is a target, not a hard record-size or memory limit: complete records can exceed it. A null size disables automatic size-based chunk flushing; it does not establish bounded buffering. Choose using throughput, recording duration and random-read latency together.

## Keep indexes for the intended reader

| Requirement | Configuration consideration |
| --- | --- |
| Sequential file-order scan | Does not require message/chunk indexes |
| Indexed time queries and message seeks | Preserve chunks and the indexes/declarations required by the selected indexed entry point |
| Indexed metadata or attachment reads | Preserve the corresponding summary indexes |
| Summary inspection | Preserve the needed statistics and repeated declarations |

`EmitSummaryRecords` selects statistics, summary indexes and repeated declarations together. Individual non-null switches override that group selection. It does not control `EmitMessageIndexes` or `EmitSummaryOffsets`. Disabling one family is not a universal “no indexes” or “no retained indexes” mode.

Use [query selection](queries.md) to decide whether sequential fallback is acceptable. For example, `UseChunks = false` with `EmitSummaryRecords = false` still permits sequential messages, but cannot serve chunk-indexed queries; time sorting with `AllowBufferedSort = false` must reject unsupported fallback. A successful query is not full-file validation.

## Choose integrity checks separately

Writer CRC options control which checksums are stored. Reader options control whether the corresponding checks are performed. CRC zero represents an absent checksum; validation cannot verify a checksum that was omitted. CRCs do not validate payload schema semantics. Keep default generation unless the workload has a concrete reason to omit checks, and see [validation](validation.md) for the read-side checks.

## Account for flushes and long recordings

`Flush()` finishes the active chunk and can increase chunk/index counts when called frequently. It neither completes MCAP nor requests durable storage. Call `Complete()` at the end; request file synchronization separately when needed.

Memory includes active chunk/codec storage, declarations, file-level indexes, application queues and the destination itself. Upstream retains a ChunkIndex for each completed chunk even with `EmitChunkIndexes = false`; that switch controls emission. Message-index disabling omits per-chunk indexes; disabling attachment/metadata indexes stops retaining those respective indexes. A growing MemoryStream retains the recording itself. No automatic segmentation or index spilling occurs.

Completion has an upstream transient summary-cloning cost. The binding then retains a shared native summary and the output; whole `GetSummary()` copies scale with summary size. Use a retained summary cursor when that access fits, and release it and the writer after use. These are not constant-memory guarantees. See [long-recording contracts](api.md#long-recordings-in-one-file) and [memory diagnostics](testing.md#single-file-writer-memory-diagnostics).

The [configuration examples](../../samples/Documentation/WritingExamples.cs) verify None/Lz4/Zstd round trips and unsupported unindexed sorting rejection. They are correctness checks, not allocation or throughput measurements. Run them using the [sample instructions](usage.md#run-repository-examples), then evaluate your own workload before tuning further.
