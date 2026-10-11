# Queries and random access

English | [简体中文](../zh-CN/queries.md)

For applications that need filtering, time ordering or partial file access after the [basic tutorial](usage.md). Choose the order and validation scope first, then choose how results are delivered. The examples use the tutorial's `path`.

## Choose an execution path

| Need | Entry | Behavior |
| --- | --- | --- |
| Scan in file order, optionally filtered | `ReadMessages` / `OpenMessages` | Sequential; adding a filter does not sort |
| Read in time order with an allowed fallback | `ReadQueryMessages` / `QueryMessages` | Ascending log time by default; may collect matches when indexes cannot serve the query |
| Reject global scan-and-sort | `QueryMessages` with `AllowBufferedSort = false` | Throws if the requested time order needs buffered fallback; file-order scans remain possible |
| Require usable indexes | `OpenIndexedMessages` | No sequential or buffered-sort fallback |

Omitted and empty options are equivalent within each entry point. Topic/time filters use exact topics and `[StartTime, EndTime)` on `LogTime`. Neither ascending nor descending time order specifies an order among equal timestamps. See the [query contracts](api.md#choosing-a-query-path) for index prerequisites and failure behavior.

## Filter while preserving file order

```csharp
var factory = new McapReaderFactory(path);
foreach (var message in factory.ReadMessages(new() { Topic = "/sample" }))
    Console.WriteLine(message.LogTime);
```

With the tutorial's file, this prints 100 then 50. Scanning is useful for incremental processing when global time order is unnecessary.

## Request time order explicitly

```csharp
using var query = factory.QueryMessages(new()
{
    Topic = "/sample",
    Order = McapReadOrder.LogTime,
    AllowBufferedSort = false
});
foreach (var message in query.ReadMessages())
    Console.WriteLine(message.LogTime);
```

This prints 50 then 100 for the indexed tutorial file. If indexes cannot provide this time order, opening throws `NotSupportedException` before collecting matches. To permit global collection and sorting, set `AllowBufferedSort = true` (the default). `MaxBufferedSortBytes` controls a local collection allowance, not all retained memory. See [local limits](api.md#binding-performance-and-local-limits).

To require indexes rather than merely prohibit sorting fallback:

```csharp
using var indexed = factory.OpenIndexedMessages(new() { Topic = "/sample" });
foreach (var message in indexed.ReadMessages())
    Console.WriteLine(message.LogTime);
```

Indexed success is not full-file validation. Malformed indexes and I/O errors can fail instead of triggering fallback.

## Revisit messages without repeated work

An indexed query session and `McapIndexedReader` serve different needs: the latter owns copied or mapped input and supports discrete seeks and chunk cursors.

| Access pattern | Choice | What is reused |
| --- | --- | --- |
| Raw records at known offsets | `factory.OpenRecordAccessor()` | Seekable input, without a complete-input copy |
| Many messages in one chunk | `McapIndexedReader.OpenChunkMessages` | One cursor's traversal |
| A known set of message locations | `SeekMessages` | Chunk loads grouped within that call |
| Repeated visits across calls | One indexed reader with a cache allowance | Retained chunks until eviction or disposal |

Use `McapIndexedReader.OpenMapped(path)` when the file can remain unchanged for every dependent cursor and lease. Choose `factory.CreateCopiedIndexedReader()` when the reader must own an independent complete input copy; its memory grows with file size. Dispose either reader. Child chunk cursors and retained leases keep their own storage references.

Individual seeks can load and validate a complete chunk. Prepared indexes reduce descriptor work but do not eliminate cache-miss decompression. Inspect cache hits and chunk loads before tuning the allowance; see [random-access contracts](api.md#complete-chunk-random-access).

Next, choose [message ownership](ownership.md) independently of access order. Use [API contracts](api.md#validation-and-recovery) to decide whether a separate full-file validation is required.

These snippets run in the [reading examples](../../samples/Documentation/ReadingExamples.cs); see the [tutorial](usage.md#run-repository-examples) for execution instructions.
