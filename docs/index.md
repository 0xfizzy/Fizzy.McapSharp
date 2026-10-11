# Fizzy.McapSharp documentation

[简体中文](zh-CN/index.md) · [GitHub repository](https://github.com/0xfizzy/Fizzy.McapSharp)

Fizzy.McapSharp reads and writes MCAP files from .NET 8. Applications define payload encoding and clock origins. Choose the entry for your task; no Rust or native ABI knowledge is needed to begin.

## Getting started

Read the [README](../README.md) for purpose and supported platforms. When ready to code, follow [your first recording](usage.md) to install from NuGet, write, read, filter and optionally validate a file.

## Advanced usage

| Current need | Guide |
| --- | --- |
| Query by time, require indexes or revisit chunks | [Queries and random access](queries.md) |
| Reuse buffers, process synchronously, retain or forward data | [Message ownership and memory choices](ownership.md) |
| Use an existing Stream, async reading or cancellation | [Streams and async](streams-async.md) |
| Supply input and perform seeks from application code | [Sans-I/O](sans-io.md) |

## Reference

- [API contracts](api.md): confirm defaults, lifetime, failure, validation and performance guarantees.
- [API reference (English)](xref:Fizzy.McapSharp): look up types, members, parameters and return values; both guide languages share this English reference.
- [Upstream API correspondence](coverage.md): locate official Rust capabilities, binding adaptations and explicit exclusions.

## Development and maintenance

- [Build and integration](development.md): source setup, host builds, package contents and consumer references.
- [Validation and diagnostics](testing.md): interoperability, allocation gates and storage performance diagnostics.
- [Native ABI](native.md) and [local patches](patches.md): implementation boundaries, extension contracts and evidence.
- [Documentation maintenance](documentation.md) and [release/recovery](release.md): documentation builds, translations, archives and publication.
- [Repository guidelines](../AGENTS.md): contribution and maintenance rules (English).

If unsure where to start, complete the [tutorial](usage.md), then choose an advanced guide for your actual workload.
