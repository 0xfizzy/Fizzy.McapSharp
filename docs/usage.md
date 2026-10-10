# Task guide and runnable examples

English | [简体中文](zh-CN/usage.md)

The [complete example](../samples/Documentation/Program.cs) runs without hardware and deletes its temporary recording. Run `./scripts/Test-Samples.ps1` from the repository root to build the native host library first and execute against current source. For an already verified complete package, use `./scripts/Test-Samples.ps1 -PackageDirectory artifacts/packages`.

## Write and validate

Register a channel, write header/payload messages and call `Complete()` explicitly. The path constructor rejects an existing file. `Dispose()` releases resources and does not complete the format. `FlushToDisk()` is a separate persistence request. Use `Validate()` for a full-file check before processing trusted-format input; payload interpretation remains the consumer's responsibility. See [writing contracts](api.md#write-a-recording).

## Read into caller buffers

The example starts with an empty destination, allocates the required size after `BufferTooSmall`, then retries the pending record. Process successful bytes before reusing the buffer. EOF is not an error. Buffer growth and setup are outside the warmed zero-managed-allocation guarantee; that guarantee does not imply zero native allocations or copies. See [buffer negotiation](api.md#read-messages-into-reusable-buffers).

## Select query behavior

Use file order for incremental scanning. Set `AllowBufferedSort = false` to reject global scan-and-sort fallback; this still permits supported indexed queries. Use `OpenIndexedMessages()` when usable indexes are mandatory. The example combines an exact topic with file order and prohibited sorting fallback. A successful indexed query does not validate the entire file. See [query paths](api.md#choosing-a-query-path).

## Borrow or retain

A visitor consumes spans synchronously and must not retain them or reenter its reader. A batch lease retains immutable backing storage after reader disposal; keep it alive until all span access finishes. The example forwards a retained batch synchronously after declaring the destination channel. Leases may retain whole chunks or mappings; payload size is not a total retained-memory bound. See [batches and leases](api.md#borrowing-batches-and-leases).

## Stream and asynchronous ownership

Use `leaveOpen: true` when the caller keeps the Stream; do not touch it while a session owns it. Await and consume each async result before another operation or disposal. Cancellation terminates that reader; it is not a reset protocol. The example uses async lease mode throughout and disposes every delivered lease. See [Stream ownership](api.md#stream-sessions-and-ownership) and [asynchronous contracts](api.md#asynchronous-records-and-errors).

Examples demonstrate behavior, not throughput measurements. Production code must handle its own payload schema and application error policy.
