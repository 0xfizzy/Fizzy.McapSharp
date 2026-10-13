# Message ownership, buffers and batch forwarding

<a id="message-ownership-and-memory-choices"></a>

English | [简体中文](../zh-CN/ownership.md)

For applications choosing when writer input can be reused and how long reader results must remain valid. Start with owned read results; use other delivery forms when buffer reuse, synchronous processing or retained sharing fits the workload. Access order is a [separate choice](queries.md).

## Writer input and buffer reuse

A writer consumes payload input before each call returns. Reuse the caller array or span afterward; do not mutate it during the call. `WriteToKnownChannel` fits registered channels and reusable buffers; complete owned messages fit existing objects; prepared descriptors snapshot control data for reuse without repeated preparation. Exact allocation guarantees remain in [API contracts](api.md#binding-performance-and-local-limits).

Writing does not transfer input ownership. Lease forwarding likewise requires keeping the lease alive until synchronous writing finishes. A batch can reference multiple backing owners without concatenating payloads. Ordinary caller-buffer reads still copy into caller storage; convenience owned results remain independent copies. See the [writing guide](writing.md) for form selection.

## Choose by lifetime

| Need | Choice | Main cost or obligation |
| --- | --- | --- |
| Keep or modify independent results | `ReadMessages()` | Allocates independent result objects and copies payloads and mutable declarations |
| Reuse application storage | `ReadNext(buffer, ...)` | Copies into caller storage; grow the buffer when required |
| Process bytes before returning | `ReadNext(visitor)` / `VisitMessages` | Borrowed spans expire on callback return; no reader reentry |
| Retain or forward shared bytes | `ReadBatchLease` | Lease control objects may allocate; backing storage stays alive until all owners release it |

Borrowing grants temporary access without ownership. A storage lease retains immutable backing storage, possibly a whole chunk or mapping. Neither choice makes memory usage proportional only to selected payload bytes. Consult [reader availability and contracts](api.md#choose-message-ownership) before selecting a delivery form.

## Reuse caller buffers

This loop uses the `path` from the [tutorial](usage.md):

```csharp
using var session = new McapReaderFactory(path).OpenMessages();
byte[] buffer = [];
while (true)
{
    var status = session.ReadNext(buffer, out var header, out var length);
    if (status == McapReadStatus.EndOfStream) break;
    if (status == McapReadStatus.BufferTooSmall)
    {
        buffer = new byte[checked((int)length)];
        continue;
    }
    Console.WriteLine($"{header.LogTime}: {System.Text.Encoding.UTF8.GetString(buffer, 0, checked((int)length))}");
}
```

An insufficient buffer leaves the same message pending for retry. Process successful bytes before reusing the buffer; empty messages are successful results, not EOF. The library's warmed caller-buffer path allocates 0 B managed, but this demonstration allocates when growing the buffer or formatting output. It still copies payload bytes. See [buffer contracts](api.md#read-messages-into-reusable-buffers).

## Borrow for immediate processing

```csharp
using var borrowed = new McapReaderFactory(path).OpenMessages();
borrowed.VisitMessages((in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
{
    Console.WriteLine($"{header.LogTime}: {payload.Length} bytes");
    return true;
});
```

Use the span only inside the callback. Do not read, inspect or dispose the same reader from that callback. Returning false stops normally after the current message. Snapshot callbacks impose additional restrictions on indexed readers and prepared indexes; see [borrowing contracts](api.md#borrowing-batches-and-leases).

## Forward a read batch into a new recording

Read the source channel descriptions, register corresponding schemas and channels on the destination writer, and build a source-to-destination Channel ID map. Replace each header's ChannelId while preserving its other fields; payloads still come from the original lease:

```csharp
var headers = new McapMessageHeader[batch.Count];
for (int i = 0; i < batch.Count; i++)
{
    var header = batch.GetHeader(i);
    headers[i] = header with { ChannelId = ids[header.ChannelId] };
}
writer.WriteBatch(batch, headers);
```

This snippet requires a live `batch`, an `ids` map covering every source channel and registered destination declarations. With matching IDs, use `WriteBatch(batch)`. The synchronous call does not transfer ownership and is not atomic; do not replay an entire failed batch. The reader can be disposed first because the independently retained lease still holds payload storage; never access concurrently with lease disposal. The [complete forwarding example](../../samples/Documentation/RecordingExamples.cs) verifies ID remapping and payload round trips for channels with and without schemas.

## Retain only what the application needs

For short pipelines, keep a batch lease alive through processing and dispose it afterward. A lease survives reader advancement, reader disposal and cache eviction. For a small payload kept much longer, copy it explicitly:

```csharp
using var retained = new McapReaderFactory(path).OpenMessages();
byte[]? savedPayload = null;
using (var batch = retained.ReadBatchLease(1))
{
    if (batch is not null)
    {
        savedPayload = new byte[batch.GetPayload(0).Length];
        batch.CopyTo(0, savedPayload);
    }
}
// savedPayload is independent of the lease and reader.
```

Copying releases no storage still referenced by another lease, parser or cache. Bound outstanding work using retained backing sizes and duration as well as batch counts. Do not access spans concurrently with lease disposal; cross-thread transfer needs caller synchronization. See [retention contracts](api.md#choosing-storage-for-retained-results).

Next, use [Streams and async](streams-async.md) for asynchronous pipelines, or consult [local limits](api.md#binding-performance-and-local-limits) and [diagnostics](testing.md) before tuning memory behavior.

These snippets run in the [reading examples](../../samples/Documentation/ReadingExamples.cs); see the [tutorial](usage.md#run-repository-examples) for execution instructions.
