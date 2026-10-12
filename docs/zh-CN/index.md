# Fizzy.McapSharp 文档

[English](../en/index.md) · [GitHub 仓库](https://github.com/0xfizzy/Fizzy.McapSharp)

在 .NET 8 / 10 应用中读写 MCAP 文件，保存带时间戳的消息、元数据和附件。支持 Windows x64 和 glibc Linux x64/ARM64，Linux 构建与验证基线为 Ubuntu 22.04。

## 入门

**第一次使用？** 从[写入和读取第一个文件](usage.md)开始：安装 NuGet 包，运行示例并查看结果。无需安装 Rust。

<a id="深入使用"></a>

<a id="写入"></a>

<a id="读取"></a>

<a id="读写协作"></a>

## 按任务选择

| 我想要…… | 指南 |
| --- | --- |
| 保存多个主题的消息，添加附件并完成文件 | [写入文件](writing.md) |
| 遍历文件，读取消息内容及主题 | [读取消息](reading.md) |
| 按主题或时间筛选，使用索引访问数据 | [查询与随机访问](queries.md) |
| 检查文件完整性，从不完整文件中读取有效消息 | [校验与恢复](validation.md) |
| 配置压缩，调整分块和索引 | [压缩、分块与索引](writer-options.md) |
| 复用读取缓冲，保留或转发消息 | [缓冲与消息转发](ownership.md) |
| 使用已有 Stream，异步读取或取消操作 | [Stream 与异步读取](streams-async.md) |
| 由应用提供输入并执行定位 | [Sans-I/O 读取](sans-io.md) |

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
