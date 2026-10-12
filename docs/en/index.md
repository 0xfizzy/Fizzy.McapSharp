# Fizzy.McapSharp documentation

[简体中文](../zh-CN/index.md) · [GitHub repository](https://github.com/0xfizzy/Fizzy.McapSharp)

Read and write MCAP files in .NET 8 / 10 applications to store timestamped messages, metadata and attachments. Supports Windows x64 and glibc Linux x64/ARM64, with an Ubuntu 22.04 build and validation baseline.

## Getting started

**First time here?** [Write and read your first file](usage.md): install the NuGet package, run the example and inspect its output. No Rust installation is needed.

<a id="advanced-usage"></a>

<a id="writing"></a>

<a id="reading"></a>

<a id="readwrite-workflows"></a>

## Choose by task

| I want to… | Guide |
| --- | --- |
| Save messages from multiple topics, add attachments and finish a file | [Write files](writing.md) |
| Iterate through messages and read their topics and contents | [Read messages](reading.md) |
| Filter by topic or time and access data through indexes | [Queries and random access](queries.md) |
| Check file integrity or read valid messages from incomplete files | [Validation and recovery](validation.md) |
| Configure compression, chunking and indexes | [Compression, chunks and indexes](writer-options.md) |
| Reuse read buffers, retain messages or forward them | [Buffers and message forwarding](ownership.md) |
| Use existing Streams, read asynchronously or cancel operations | [Streams and asynchronous reading](streams-async.md) |
| Supply input and perform seeks from application code | [Sans-I/O reading](sans-io.md) |

## Reference

- [API contracts](api.md): confirm defaults, lifetime, failure, validation and performance guarantees.
- [API reference (English)](xref:Fizzy.McapSharp): look up types, members, parameters and return values; both guide languages share this English reference.
- [Upstream API correspondence](coverage.md): locate official Rust capabilities, binding adaptations and explicit exclusions.

## Development and maintenance

- [Build and integration](development.md): source setup, host builds, package contents and consumer references.
- [Validation and diagnostics](testing.md): interoperability, allocation gates and storage performance diagnostics.
- [Native ABI](native.md) and [local patches](patches.md): implementation boundaries, extension contracts and evidence.
- [Documentation maintenance](documentation.md) and [release/recovery](release.md): documentation builds, translations, archives and publication.
- [Repository guidelines](../../AGENTS.md): contribution and maintenance rules (English).
