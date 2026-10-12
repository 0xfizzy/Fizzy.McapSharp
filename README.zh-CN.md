# Fizzy.McapSharp

[![NuGet](https://img.shields.io/nuget/v/Fizzy.McapSharp.svg?style=flat)](https://www.nuget.org/packages/Fizzy.McapSharp/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat)](LICENSE)
[![.NET 8](https://img.shields.io/badge/.NET-8-512BD4.svg?style=flat)](https://dotnet.microsoft.com/)
[![Platforms: Windows and Linux](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-blue.svg?style=flat)](docs/zh-CN/api.md)

[English](README.md) | 简体中文

基于官方 Rust `mcap` 实现的 .NET 8 文件读写库。[MCAP](https://mcap.dev/spec) 是用于保存带时间戳消息及其 Schema、Channel 等信息的容器格式。本库支持消息、元数据和附件读写，以及压缩、时间过滤和完整性校验；载荷（payload）编码与时钟基准由应用定义。

支持 Windows x64 和 glibc Linux x64/ARM64；Linux 构建及验证基线为 Ubuntu 22.04。详细支持范围见 [API 契约](docs/zh-CN/api.md)。

## 快速开始

### 安装

在 .NET 8 应用目录中运行：

```powershell
dotnet add package Fizzy.McapSharp
```

### 读写消息

以下示例使用 C# 12。目标文件必须不存在；时间戳单位为纳秒，起点由应用约定。

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

必须显式调用 `Complete()` 完成文件格式；`Dispose()` 只释放资源。需要请求文件持久化时，在完成后、释放前调用 `FlushToDisk()`。

需要完整文件校验时，另行调用 `reader.Validate()`；它会单独扫描整个文件。普通读取不替代完整校验，也不会验证载荷的业务含义。

<a id="文档入口"></a>

## 下一步

- [入门教程](docs/zh-CN/usage.md)：从创建项目开始，写入和读取第一个文件。
- [任务指南](docs/zh-CN/index.md)：按写入、读取、查询或校验等需求选择指南。
- [API 参考与契约](docs/zh-CN/index.md#参考)：查找类型和成员，确认默认行为、生命周期及错误处理。

本项目自身的所有代码均由 AI 生成。第三方依赖及随仓库分发的上游代码保留其原作者归属和许可证。

本项目采用 [MIT 许可证](LICENSE)；第三方依赖见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
