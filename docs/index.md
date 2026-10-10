# Fizzy.McapSharp documentation

[简体中文](zh-CN/index.md)

Fizzy.McapSharp provides general MCAP file operations for .NET 8. Payload encoding and clock semantics belong to your application.

## Getting started and usage

- [Task guide and runnable examples](usage.md): write, validate and read your first recording.
- [API contracts](api.md): choose [message ownership](api.md#choose-message-ownership) and [access patterns](api.md#choose-an-access-pattern), then review completion, disposal, failure and allocation guarantees.

## Development and internals

- [Development](development.md): build and validate current source or a complete package.
- [Native ABI](native.md): native interoperability and memory boundaries.
- [Upstream coverage](coverage.md): correspondence with the official Rust API.
- [Local patches](patches.md): permitted extensions and their evidence.

## Documentation maintenance

Both guide languages share the English generated **API reference (English)** in the site navigation. Consult it for individual member contracts and use the guides for cross-API behavior.

Use [documentation maintenance](documentation.md) to build the site and maintain translations, and the [release SOP](release.md) to archive and publish documentation.
