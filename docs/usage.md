# Task guide and runnable examples

English | [简体中文](zh-CN/usage.md)

The [complete example](../samples/Documentation/Program.cs) runs without hardware and deletes its temporary recording. Run `./scripts/Test-Samples.ps1` from the repository root to build the native host library first and execute against current source. For an already verified complete package, use `./scripts/Test-Samples.ps1 -PackageDirectory artifacts/packages`.

## Write and validate

Register a channel, write header/payload messages and call `Complete()` explicitly. The path constructor rejects an existing file. `Dispose()` releases resources and does not complete the format. `FlushToDisk()` is a separate persistence request. Use `Validate()` for a full-file check before processing trusted-format input; payload interpretation remains the consumer's responsibility. See [writing contracts](api.md#write-a-recording).

## Read into caller buffers

The example starts with an empty destination, allocates the required size after `BufferTooSmall`, then retries the pending record. Process successful bytes before reusing the buffer. EOF is not an error. Buffer growth and setup are outside the warmed zero-managed-allocation guarantee; that guarantee does not imply zero native allocations or copies. See [buffer negotiation](api.md#read-messages-into-reusable-buffers).

## Select query behavior

To filter by topic/time while preserving recording order, call `OpenMessages(new() { Topic = "/sample" })`; add `StartTime` / `EndTime` for an inclusive/exclusive LogTime interval. File order needs no fallback-policy switch. Omitted and empty filters both use file order; adding Topic or Topics does not change it.

For time order without global scan-and-sort, use `QueryMessages(new() { Order = McapReadOrder.LogTime, AllowBufferedSort = false })`. If the query cannot use indexes, opening throws `NotSupportedException` before collecting matches. With the default `AllowBufferedSort = true`, it may collect all matches before returning the session. `ReverseLogTime` has the same fallback policy. Neither time order specifies the order of equal timestamps.

Use `OpenIndexedMessages()` on the file factory when indexes are mandatory; disabling buffered sorting alone still permits file-order scans. Indexed reading can buffer chunks, and successful queries do not validate the entire file. The example checks file versus default query order, supported time ordering with fallback disabled, and rejection on an unindexed source. See [query paths and local allowances](api.md#choosing-a-query-path).

## Borrow or retain

A visitor consumes spans synchronously and must not retain them or reenter its reader. A batch lease retains immutable backing storage after reader disposal; keep it alive until all span access finishes. The example forwards a retained batch synchronously after declaring the destination channel. Leases may retain whole chunks or mappings; payload size is not a total retained-memory bound. See [batches and leases](api.md#borrowing-batches-and-leases).

Choose `ReadMessages()` when you need independent mutable results without managing a lease. For a few payloads retained long term, [copy the needed bytes before disposing the lease](api.md#choosing-storage-for-retained-results). Caller buffers copy too, even on a zero-managed-allocation path. A callback avoids the final delivery copy but cannot be retained; a lease avoids that copy while retaining backing storage. These are different costs, not guarantees of low total memory. Check [reader availability](api.md#choose-message-ownership) before choosing a delivery form.

## Stream and asynchronous ownership

Use `leaveOpen: true` when the caller keeps the Stream; do not touch it while a session owns it. Await and consume each async result before another operation or disposal. Cancellation terminates that reader; it is not a reset protocol. The example uses `McapAsyncMessageReader` throughout and disposes every delivered lease. See [Stream ownership](api.md#stream-sessions-and-ownership) and [asynchronous contracts](api.md#asynchronous-records-and-errors).

Examples demonstrate behavior, not throughput measurements. Production code must handle its own payload schema and application error policy.
