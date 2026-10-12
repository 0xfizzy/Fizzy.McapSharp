# Fizzy.McapSharp

[![NuGet](https://img.shields.io/nuget/v/Fizzy.McapSharp.svg?style=flat)](https://www.nuget.org/packages/Fizzy.McapSharp/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat)](LICENSE)
[![.NET 8 / 10](https://img.shields.io/badge/.NET-8%20%2F%2010-512BD4.svg?style=flat)](https://dotnet.microsoft.com/)
[![Platforms: Windows and Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-blue.svg?style=flat)](docs/en/api.md)

English | [简体中文](README.zh-CN.md)

.NET 8 / 10 bindings to the official Rust `mcap` implementation. [MCAP](https://mcap.dev/spec) is a container format for timestamped messages and their schemas and channels. Read and write messages, metadata and attachments, with compression, time filtering and integrity validation. Applications define payload encoding and clock origins.

Supports Windows x64 and glibc Linux x64/ARM64, with an Ubuntu 22.04 Linux build and validation baseline. See [API contracts](docs/en/api.md) for the full support scope.

## Quick start

### Install

Run from a .NET 8 / 10 application directory:

```powershell
dotnet add package Fizzy.McapSharp
```

### Write and read messages

The example uses C# 12. The output path must not exist; timestamps are in nanoseconds with an application-defined origin.

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"sample-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var channel = writer.RegisterChannel("/sample", "json");
    writer.WriteToKnownChannel(new McapMessageHeader(channel, 0, 100, 100), "{}"u8);
    writer.Complete();
}

var reader = new McapReaderFactory(path);
foreach (var message in reader.ReadMessages())
    Console.WriteLine($"{message.LogTime}: {System.Text.Encoding.UTF8.GetString(message.Data)}");
```

Call `Complete()` explicitly to finish the format; `Dispose()` only releases resources. To request file persistence, call `FlushToDisk()` after completion and before disposal.

Call `reader.Validate()` separately when a full-file check is needed; it performs an additional scan of the entire file. Ordinary reading does not replace full validation or validate business payload semantics.

<a id="documentation"></a>

## Next steps

- [Getting started](docs/en/usage.md): create a project, then write and read your first file.
- [Task guides](docs/en/index.md): find help with writing, reading, querying or validating files.
- [API reference and contracts](docs/en/index.md#reference): look up types and members, defaults, lifetimes and error handling.

All code authored for this project is AI-generated. Third-party dependencies and vendored upstream code retain their original authorship and licenses.

Licensed under [MIT](LICENSE). See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for third-party dependencies.
