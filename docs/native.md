# Native ABI and memory boundaries

English | [简体中文](native.zh-CN.md)

.NET calls a Rust `cdylib` through Cdecl P/Invoke, using the official `mcap` crate 0.25.0 directly, without a C++ layer. The implementation is in [lib.rs](../native/src/lib.rs), with managed declarations in [Native.cs](../src/Fizzy.McapSharp/Native.cs). The native asset is `fizzy_mcap_native.dll` for Windows MSVC x64.

## Version and entry points

This is a private ABI, not a stable public interface for third-party callers. `fm_abi_version()` currently returns 1; the managed wrapper checks it when constructing a reader or writer.

| Entry point | Responsibility |
| --- | --- |
| `fm_writer_open` / `fm_writer_call` / `fm_writer_free` | Create, operate on, and release a writer |
| `fm_reader_open` / `fm_reader_next` / `fm_reader_free` | Create a reader for one enumeration, retrieve records, and release it |
| `fm_validate` | Validate the entire file; return the record count in the response scalar |
| `fm_buffer_free` | Release a returned buffer |

`fm_writer_call` operation codes are: 1 register schema, 2 register channel, 3 write message, 4 write metadata, 5 write attachment, 6 flush, and 7 complete. Update Rust dispatch and C# calls together when changing them.

## Data exchange

Request control headers are length-delimited UTF-8 JSON. Binary data is passed separately as a pointer and length and consumed synchronously. Control JSON is an ABI implementation detail; it does not become a user message schema.

Responses use `repr(C)` / `LayoutKind.Sequential`, with a JSON pointer and length, a binary pointer and length, and a `u64` scalar, in that order. Lengths are native `usize` / managed `nuint`. Successful control headers contain JSON; error responses use the same buffer for UTF-8 error text.

Status 0 means success, 1 means reader EOF, and negative values mean failure. `Native.Consume` copies the content and frees both buffers through `fm_buffer_free` in a finally block, including on error. Do not free Rust buffers with a managed allocator or expose borrowed native memory through public models.

## Handles, concurrency, and disposal

Handles are opaque native objects and must be freed exactly once by their matching free function. Managed SafeHandle prevents disposal during P/Invoke. The C# layer locks the writer; each reader handle belongs to one enumerator and does not support concurrent calls.

Reader iterators borrow fixed mapping and summary allocations. Internally extended reference lifetimes are valid only within the reader; fields must be dropped in this order: iterator, summary, mapping, file. Moving the reader must not move borrowed allocations. Returned data is copied; mapping references must not escape the reader.

Windows opens allow read sharing only, preventing ordinary concurrent writes and deletion. Pre-existing writable mappings remain outside this protection. Full validation limits record lengths to the mapped file size; this is not a general memory quota and does not eliminate resource risks from untrusted input.

Writer disposal uses the upstream `into_inner` path to avoid implicitly finishing through upstream Drop. Complete explicitly finishes the file and calls `sync_all`; a failed writer cannot continue writing.

## Error boundaries

Fallible entry points use `catch_unwind` to convert Rust panics to error status. Handle release also catches destructor panics. Allocator aborts cannot become .NET exceptions. Callers must supply valid pointers, lengths, and handles created by this library; arbitrary external native pointers are unsupported.

Incompatible ABI changes require a matching version check update. See [api.md](api.md) for public lifetime contracts and [development.md](development.md) for validation steps.
