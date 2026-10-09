# 官方 Rust API 覆盖

[English](coverage.md) | 简体中文

本文用于查找官方 Rust 能力的 .NET 入口并理解适配差异。基线是 [Cargo.toml](../native/Cargo.toml) 锁定的 `mcap` 依赖。所有权、分配和使用方式的选择见 [API 指南](api.zh-CN.md#选择消息所有权)。

## 如何阅读覆盖关系

下表按能力组织，不逐条展开声明。一个托管入口可以覆盖多个 Rust 构造函数、builder 方法或迭代操作。**适配**表示通过托管类型或组合操作提供官方能力，不承诺签名、所有权或全部行为相同。**替代**表示通过另一官方能力提供相近的托管功能。**未暴露**属于明确排除，不能算作已实现。

| 来源 | 职责 |
| --- | --- |
| [upstream-api.json](upstream-api.json) | 未修改上游的固定声明基线；不得为接纳本地扩展而修改 |
| [api-coverage.json](api-coverage.json) | 符号级映射、能力分组、映射类别、来源、实现／测试引用及排除原因 |
| 本文 | 能力导航和重要适配差异；详细用法由 API 指南维护 |

审查清单包含 344 项官方声明和 18 项本地扩展声明，涵盖类型、方法、字段、常量和错误变体；这些数量不是已实现操作数或通过测试数。可选 Tokio 声明也在清单内，但绑定不调用 Tokio。

## 写入（`writing`）

托管入口位于 [McapWriter](../src/McapWriter.cs) 及其 partial 实现。

| 官方 Rust API | .NET 入口 | 映射 | 关键差异或约束 |
| --- | --- | --- | --- |
| `WriteOptions`、`Writer::new/with_options` | `McapWriterOptions`、`McapWriter` 构造函数 | 适配 | Builder 配置转为属性；摘要总开关先于显式单项覆盖 |
| `Writer::add_schema/add_schema_with_id`、`add_channel/add_channel_with_id` | `RegisterSchema`、`RegisterChannel` 重载 | 适配 | ID 与二进制载荷分离；prepared 描述符属于绑定扩展 |
| `Writer::write` | `WriteMessage(McapMessage)`、prepared channel 重载 | 适配 | 调用上游自动声明；准备操作快照化可变描述 |
| `Writer::write_to_known_channel` | `WriteMessage(in header, span)` | 适配 | 同步消费借用载荷，无托管 payload 副本 |
| `Writer::attach`、`start_attachment/put_attachment_bytes/finish_attachment` | `WriteAttachment`、`StartAttachment/WriteAttachmentBytes/FinishAttachment` | 适配 | 分段附件必须满足声明的精确长度 |
| `Writer::write_metadata`、`write_private_record`、`PrivateRecordOptions` | `WriteMetadata`、`WritePrivateRecord` | 适配 | 私有记录位置选项转为 `includeInChunks` |
| `Writer::flush`、`finish`、`into_inner` | `Flush`、`Complete/GetSummary`、`IntoInner` | 适配 | 完成和摘要获取分离；释放不隐式完成。`FlushToDisk` 属于绑定扩展 |

默认值遵循上游 writer，包括 Zstd、1 MiB 目标 chunk 和上游 library 标识。路径创建保护、配置的安全拒绝白名单以外的终止性失败属于托管契约。完整规则见[写入录制](api.zh-CN.md#写入录制)。

## 顺序读取（`sequential-reading`）

| 官方 Rust API | .NET 入口 | 映射 | 关键差异或约束 |
| --- | --- | --- | --- |
| `read::LinearReader`、`Options`、`sans_magic` | `McapReadCursor` 的 TopLevelRecords/ExpandedRecordsWithoutMagic 模式 | 适配 | 官方 Sans-I/O 配置匹配切片读取器；输入复制或显式映射 |
| `read::ChunkReader` | `McapReadCursor` 的 ChunkRecords 模式 | 适配 | 接收 Chunk 记录体并惰性推进 |
| `read::ChunkFlattener` | ExpandedRecords 模式 | 适配 | 将 chunk 展开为记录 |
| `read::RawMessageStream`、`RawMessage`、`get_channel` | RawMessages 模式、header/payload 读取、`GetChannel` | 适配 | 保留已遇到的声明；借用 Rust 迭代器不跨越 ABI |
| `read::MessageStream` | Messages 模式和自有消息枚举 | 适配 | 自有结果复制 payload 及可变声明；其他交付方式的所有权不同 |

[McapReadCursor](../src/McapReadCursor.cs) 将这些切片接口统一为模式。`McapReaderFactory.OpenMessages/OpenRecords` 另外提供由官方解析驱动的文件／Stream 会话。顺序文件读取、输入所有权和交付所有权分别选择，详见 [API 指南](api.zh-CN.md)。

## 摘要与随机访问（`random-access`）

| 官方 Rust API | .NET 入口 | 映射 | 关键差异或约束 |
| --- | --- | --- | --- |
| `Summary`、`Summary::read` 及摘要字段 | `McapIndexSnapshot`、`GetSummary`、`OpenSummaryRecords` | 适配 | Snapshot 拥有输入副本或映射；托管摘要和记录游标是不同表示形式 |
| `Summary::stream_chunk` | `OpenChunkReader`、`ReadChunkMessages` | 适配 | 独立惰性游标共享输入和摘要，在 snapshot 释放后仍有效 |
| `Summary::seek_message` | `SeekMessage` 重载 | 适配 | 绑定加载／校验完整 chunk，可能比上游前缀定位更早报告尾部损坏 |
| `Summary::read_message_indexes` | `ReadMessageIndexes` | 适配 | 调用方缓冲形式使用 18 字节打包行 |
| `read::attachment`、`read::metadata` | `ReadAttachment`、`ReadMetadata` | 适配 | 使用调用方提供的索引并验证源范围 |

入口见 [McapIndexSnapshot](../src/McapIndexSnapshot.cs)。Prepared index、分组 seek 和缓存保留属于绑定扩展，不是新增官方方法；复用范围见[随机访问](api.zh-CN.md#完整-chunk-随机访问)。索引读取成功不代表完整文件校验。

## Sans-I/O（`sans-io`）

| 官方 Rust API | .NET 入口 | 映射 | 关键差异或约束 |
| --- | --- | --- | --- |
| `sans_io::LinearReader`、`LinearReaderOptions`、`LinearReadEvent` | `McapSansIoReader.CreateLinear`、`McapReaderOptions`、`NextEvent/SupplyInput` | 适配 | 值事件和调用方缓冲替代 Rust 事件借用及可写输入切片 |
| `sans_io::SummaryReader`、选项及事件 | `CreateSummary`、`McapSummaryReaderOptions`、输入／定位通知及摘要访问 | 适配 | 调用方驱动 I/O；完成后摘要可用于索引读取 |
| `sans_io::IndexedReader`、选项、事件及 `ReadOrder` | `CreateIndexed`、`McapQuery`、`McapReadOrder`、索引控制操作 | 适配 | 直接索引引擎要求索引，不进行扫描排序回退 |

具体事件与控制项见 [SansIo.cs](../src/SansIo.cs) 及符号清单。可选 CRC 检查默认遵循上游配置。共享事件和直接填充所有权扩展在下文单列。

## 记录、工具与错误（`records-and-errors`）

| 官方 Rust API | .NET 入口 | 映射 | 关键差异或约束 |
| --- | --- | --- | --- |
| `Schema`、`Channel`、`Message`、`Attachment`、`Compression` | 对应 `Mcap*` 模型与 `McapCompression` | 适配 | 托管所有权替代 `Cow`、`Arc` 和 Rust 生命周期 |
| `records::*`、操作码及格式常量 | 自有记录模型、`McapRecordView.Fields`、`McapOpcode`、`McapFormat` | 适配 | 字段和变体可通过视图或自有模型访问，不一定有独立方法 |
| `read::parse_record`、`read::footer`、chunk 数据偏移辅助方法 | `McapRecords.Parse`、`McapRecordView.Parse`、`ReadFooter`、`GetCompressedDataOffset` | 适配 | 自有解析复制数据；视图遵循调用方内存生命周期 |
| `McapError`、`McapResult` | `McapErrorKind`、`McapException.Kind/Details`、返回值和异常 | 适配 | 结构化错误转换替代 Rust result；.NET 参数／释放／Stream 异常保留各自契约 |

## 异步与未暴露接口

| 官方 Rust API | .NET 入口 | 映射 | 原因或约束 |
| --- | --- | --- | --- |
| 可选 `tokio::LinearReader`（`async`） | `McapAsyncReader.ReadNextRecordAsync` | 替代 | .NET 异步 I/O 驱动官方 Sans-I/O，不调用 Tokio 本身；取消和挂起遵循托管契约 |
| `sans_io::Decompressor`、`DecompressResult`（`unexposed`） | 无 | 未暴露 | 上游没有自定义解压器注册入口；未实现该 trait 的托管版本 |

功能替代不代表移植特定运行时接口。实现见[异步 reader](../src/McapAsyncReader.cs)。

异步消息 lease 是线性引擎之上的绑定能力。消费在途读取后，`GetChannelDescription` 和 `GetSchemaDescription` 可查询 lease 模式已遇到的不可变声明；这不增加上游 API 或本地补丁。

## 本地扩展与绑定能力

清单中 `origin: local-extension`、分组为 `local-extensions` 的条目表示本地补丁声明，不计入官方数量。纯绑定层托管能力不一定增加 Rust 公共声明，也不加入官方基线。

| 层次 | 能力 | 契约与证据 |
| --- | --- | --- |
| 原生本地补丁 | 共享存储／事件、直接填充所有权、`shares_backing` | 发布范围不可变及保留所有权；指针、推进、释放和淘汰检查 |
| 原生本地补丁 | `Writer::contains_channel`；输出取出后的释放 | 批次预检及释放不隐式完成；writer／lease 测试 |
| 托管／原生绑定 | Prepared 描述符、借用交付、批次、lease 及转写 | 显式所有权与已完成前缀失败规则；`BatchTests`、`LeaseTests`、`LeaseWriteTests`、分配门禁 |
| 托管／原生绑定 | 缓存、分组 seek、回退排序及自适应存储 | 局部额度、复用范围及保留；`BatchSeekTests`、`SortStorageTests`、原生诊断 |
| 托管／原生绑定 | 异步调度、输入预留、`Complete`／`FlushToDisk` 分离 | 托管挂起／生命周期及持久化契约；异步、预留和 writer 完成测试 |

补丁动机和允许范围由 [patches.zh-CN.md](patches.zh-CN.md) 维护，ABI 所有权由 [native.zh-CN.md](native.zh-CN.md) 维护，公共用法与性能保证由 [api.zh-CN.md](api.zh-CN.md) 维护。

## 核验与维护

修改映射后运行 `python scripts/check_api_coverage.py`。脚本对照所用原生源码检查符号集合和种类，通过固定基线区分官方／本地来源，并检查映射类别、分组、必要原因和引用文件。它不验证每条映射描述的语义正确性，也不证明两种实现行为相同；这些需要审查及行为测试。

行为证据单独评估：固定 conformance 用例、.NET／Python 互操作、独立未修改上游对比、生命周期／重试测试及分配门禁分别验证不同性质。仅对补丁源码测试不构成独立上游证据；绑定诊断不证明总内存上限，也不约束 codec 分配。命令、夹具数量和限制集中维护在 [development.zh-CN.md](development.zh-CN.md#外部契约测试套件)。

审查某项能力时，先定位上述分组，再到审查清单查找精确 `rust` 符号，最后检查所映射源码及证据。新增本地能力时保持官方基线不变；明确记录替代和排除项，不将其计作直接实现。
