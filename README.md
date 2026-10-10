# Fizzy.McapSharp

[![NuGet](https://img.shields.io/nuget/v/Fizzy.McapSharp.svg?style=flat)](https://www.nuget.org/packages/Fizzy.McapSharp/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat)](LICENSE)
[![.NET 8](https://img.shields.io/badge/.NET-8-512BD4.svg?style=flat)](https://dotnet.microsoft.com/)
[![Platforms: Windows and Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-blue.svg?style=flat)](docs/api.md)

English | [简体中文](README.zh-CN.md)

.NET bindings to the official Rust `mcap` implementation. Read and write messages, schemas, channels, metadata, and attachments, with compression, time filtering, and integrity validation.

All code authored for this project is AI-generated. Third-party dependencies and vendored upstream code retain their original authorship and licenses.

## Quick start

Add the package to a .NET 8 application. Supported processes: Windows x64 and glibc Linux x64/ARM64 (Ubuntu 22.04 or newer validation baseline):

```powershell
dotnet add package Fizzy.McapSharp
```

The example uses C# 12. The output path must not exist; timestamps are in nanoseconds.

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"sample-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var channel = writer.RegisterChannel("/sample", "json");
    writer.WriteMessage(new McapMessageHeader(channel, 0, 100, 100), "{}"u8);
    writer.Complete();
}

var reader = new McapReaderFactory(path);
reader.Validate();
foreach (var message in reader.ReadMessages(new() { Topic = "/sample" }))
    Console.WriteLine($"{message.LogTime}: {System.Text.Encoding.UTF8.GetString(message.Data)}");
```

Call `Complete()` explicitly to finish the format, then optionally `FlushToDisk()` to request file persistence; `Dispose()` only releases resources. `ReadMessages()` does not replace full-file validation.

Large-payload pipelines can use borrowed callbacks, batch reads/writes and stable storage leases; start with [choosing message ownership](docs/api.md#choose-message-ownership), then review lifetimes and local cache/sort allowances.

## Documentation

- [API, lifetime, and data ownership](docs/api.md)
- [Building, testing, and consumer integration](docs/development.md)
- [Native ABI and memory boundaries](docs/native.md)
- [Repository guidelines](AGENTS.md)

Licensed under [MIT](LICENSE). See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for third-party dependencies.

- [Documentation home](docs/index.md)
- [Release SOP](docs/release.md)
