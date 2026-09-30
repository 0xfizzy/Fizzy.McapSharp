# Fizzy.McapSharp

[![NuGet](https://img.shields.io/nuget/v/Fizzy.McapSharp.svg?style=flat)](https://www.nuget.org/packages/Fizzy.McapSharp/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat)](LICENSE)
[![.NET 8](https://img.shields.io/badge/.NET-8-512BD4.svg?style=flat)](https://dotnet.microsoft.com/)
[![Platforms: Windows and Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-blue.svg?style=flat)](docs/api.zh-CN.md)

[English](README.md) | 简体中文

基于官方 Rust `mcap` 实现的 .NET 8 文件读写库，支持消息、Schema、Channel、元数据、附件、压缩、时间过滤和完整性校验。

## 快速开始

在 .NET 8 应用中添加依赖。支持 Windows x64 和 glibc Linux x64/ARM64 进程，Linux 验证基线为 Ubuntu 22.04 及以上：

```powershell
dotnet add package Fizzy.McapSharp
```

以下示例使用 C# 12。目标文件必须不存在，时间戳单位为纳秒。

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"sample-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var channel = writer.RegisterChannel("/sample", "json");
    writer.WriteMessage(new McapMessageHeader(channel, 0, 100, 100), "{}"u8);
    writer.Complete();
}

var reader = new McapReader(path);
reader.Validate();
foreach (var message in reader.ReadMessages(new() { Topic = "/sample" }))
    Console.WriteLine($"{message.LogTime}: {System.Text.Encoding.UTF8.GetString(message.Data)}");
```

必须显式调用 `Complete()` 完成格式，再按需调用 `FlushToDisk()` 请求文件持久化；`Dispose()` 只释放资源。`ReadMessages()` 不替代完整文件校验。

## 文档入口

- [API、生命周期与数据所有权](docs/api.zh-CN.md)
- [源码构建、测试与消费者集成](docs/development.zh-CN.md)
- [原生 ABI 与内存边界](docs/native.zh-CN.md)
- [仓库维护约定](AGENTS.md)

本项目采用 [MIT 许可证](LICENSE)；第三方依赖见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
