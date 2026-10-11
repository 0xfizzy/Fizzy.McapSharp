# Fizzy.McapSharp 文档

[English](../index.md) · [GitHub 仓库](https://github.com/0xfizzy/Fizzy.McapSharp)

Fizzy.McapSharp 为 .NET 8 提供 MCAP 文件读写。载荷编码和时钟基准由应用决定。按当前任务选择入口，无需先了解 Rust 或原生 ABI。

## 入门

首次接触项目，先看 [README](../../README.zh-CN.md)了解用途和支持平台；准备开始编码，阅读[第一个文件](usage.md)，通过 NuGet 完成写入、读取、过滤和可选校验。

## 深入使用

| 当前需求 | 指南 |
| --- | --- |
| 按时间查询、强制索引、重复访问 Chunk | [查询与随机访问](queries.md) |
| 复用缓冲、同步处理、跨操作保留或转发数据 | [消息所有权与内存取舍](ownership.md) |
| 使用已有 Stream、异步读取或取消 | [Stream 与异步](streams-async.md) |
| 由应用自行提供输入和执行定位 | [Sans-I/O](sans-io.md) |

## 参考

- [API 契约](api.md)：确认默认值、生命周期、失败、验证和性能保证。
- [API reference (English)](xref:Fizzy.McapSharp)：查询类型、成员、参数和返回值；中英文指南共用英文成员参考。
- [上游 API 对应关系](coverage.md)：核对官方 Rust 能力、绑定适配及明确排除项。

## 开发与维护

- [构建与集成](development.md)：源码环境、本机构建、包内容及消费项目引用。
- [验证与诊断](testing.md)：互操作、分配门禁和存储性能诊断。
- [原生 ABI](native.md)与[本地补丁](patches.md)：实现边界、扩展契约及证据。
- [文档维护](documentation.md)与[发布恢复](release.md)：文档构建、翻译、归档及发布操作。
- [仓库约定](../../AGENTS.md)：贡献和维护规则（英文）。

不确定从哪里开始时，先完成[入门教程](usage.md)，再按实际需求选择深入指南。
