# Native ABI

The private version-1 ABI calls the official `mcap` Rust crate 0.25.0 directly through a Rust `cdylib`. There is no C++ layer. Cargo.lock pins the complete dependency graph. The Windows MSVC target is x86_64-pc-windows-msvc.

`fm_abi_version` must equal 1. Handles are opaque, allocated by native open functions and released exactly once through the matching free function. Managed SafeHandle prevents release during P/Invoke calls. A writer is serialized in managed code; a reader handle belongs to one enumerator and is not thread-safe.

Requests are length-delimited UTF-8 JSON control headers. Binary payloads are separate pointer/length inputs, synchronously consumed. Response contains JSON pointer/length, binary pointer/length and a u64 scalar. This control representation is private, versioned by the ABI and never appears as an MCAP payload schema. Returned buffers are freed with `fm_buffer_free`; C# consumes them in a finally block. No Rust layout or native memory ownership escapes into public .NET models.

Status 0 means success, 1 means reader EOF, negative means an error whose UTF-8 description is in the response. All fallible entrypoints catch Rust panics. Allocator aborts cannot be converted to exceptions. Invalid native pointers from callers other than the managed wrapper are not supported.

Reader mapping and summary allocations stay fixed while iterators borrow them. Reader field drop order releases the iterator before these allocations and before the open file. The ABI copies returned data; no mapped memory escapes. Writer disposal uses the upstream `into_inner` path specifically to avoid the upstream Drop implementation silently finishing a recording.
