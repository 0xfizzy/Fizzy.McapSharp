# 官方 Rust API 覆盖

[English](coverage.md) | 简体中文

本文供选择官方 Rust 等价入口的 API 使用者参考，基线是 `native/Cargo.toml` 锁定的 `mcap` 版本。[声明清单](api-coverage.json) 记录 344 项公共声明，包括方法、字段、错误变体和常量，以及托管/原生映射与验证引用。`python scripts/check_api_coverage.py` 对照锁定的 Cargo 源码检查清单；清单完整性不等同于行为测试。

| 官方能力 | 托管入口 | 验证 |
| --- | --- | --- |
| `Writer::write` | 完整消息和预准备通道的 `WriteMessage` 重载，均直接调用上游 write | 自动声明、不可变快照、分配门禁 |
| 其他 Writer 方法 | 注册、已知通道消息、附件、Metadata、私有记录、Flush、Finish、IntoInner | 往返、所有权、互操作、分配测试 |
| `WriteOptions` | `McapWriterOptions`，先总开关后显式单项覆盖 | 原生默认值/配置差分测试 |
| 官方切片读取器 | `McapBufferReader` 各模式，使用配置匹配切片入口的官方 Sans-I/O | 切片、记录模型、分配测试 |
| Summary 读取、Chunk 消息、消息定位与索引 | `McapIndexSnapshot` 对应操作 | 随机读取及重试测试 |
| 附件、Metadata、Footer、记录解析 | 索引快照、`McapRecords`、`McapRecordView` | 记录模型、Footer、CRC、分配测试 |
| 所有标准记录、操作码、常量 | 自有记录模型、调用方内存字段视图、`McapOpcode`、`McapFormat` | 标准记录模型及未知记录保留 |
| Sans-I/O 线性、摘要、索引读取及选项/事件 | `McapSansIoReader`、选项与值类型事件 | 输入/定位事件、多 Topic 排序、限制、重试 |
| 公共解压器 trait | 未暴露：上游没有自定义解压器注册入口 | 仅源码审查，无托管实现 |
| 可选 Tokio 线性读取 | .NET 异步 I/O 驱动官方 Sans-I/O 的 `McapAsyncReader` | 取消、所有权、强制挂起分配门禁 |
| 全部错误变体与结果类型 | `McapException.Kind/Details`、返回值和异常 | 原生穷尽匹配及字段测试 |

Rust 生命周期、`Cow`、`Arc`、迭代器及 builder 映射为自有结果、调用方内存视图、可释放会话和 .NET 选项属性，不公开 Rust 布局或句柄。缓冲区适配器复制输入并惰性解析，索引快照复制数据源并与独立惰性 Chunk 游标共享；排序回退收集选中消息，可用 AllowBufferedSort=false 禁止；这些所有权选择需要与输入/结果成比例的原生内存，需要流式读取时应选用增量会话。

默认值遵循对应上游 API：Writer 使用 Zstd、1 MiB Chunk 和上游 Library；顺序消息采用文件顺序，索引查询采用 LogTime；Sans-I/O 可选 CRC 默认关闭。直接切片读取器保留各自上游默认值。路径创建保护、终止性失败、显式完成及释放不自动完成保留为安全差异。

零托管分配请选预准备写入、调用方缓冲区读取、记录视图、摘要游标或可复用异步读取器；自有对象便利接口允许分配。入口选择参见 [API 契约](api.zh-CN.md)、[ABI](native.zh-CN.md) 和[验证](development.zh-CN.md)。
