# 读取消息

<a id="读取录制"></a>

[English](../en/reading.md) | 简体中文

使用 `ReadMessages()` 按文件顺序遍历消息，每条消息包含独立的数据副本。本指南读取[写入文件](writing.md)生成的文件；项目准备见[入门教程](usage.md)。

## 读取多主题文件

下面是独立的 C# 12 程序；将写入指南打印的路径传给 `dotnet run -- <path>`。

```csharp
using Fizzy.McapSharp;
using System.Text;

if (args.Length != 1) throw new ArgumentException("Supply the recording path.");
var factory = new McapReaderFactory(args[0]);
foreach (var message in factory.ReadMessages())
    Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

输出按文件顺序排列，最后一条消息的载荷为空：

```text
/values 100: {"value":1}
/events 50: started
/events 110:
```

## 理解声明与消息

`McapMessage` 将消息头的序号、纳秒时间戳与已解析的 Channel 和载荷字节组合起来。Channel 包含可选 Schema 和元数据。每条消息的数据副本可以独立修改，这类结果称为自有结果。库解释 MCAP 容器，不解释应用的载荷编码或时钟基准。

## 按主题和时间过滤

将下面的代码追加到上例。输出为 `/events 50: started`。过滤使用 `LogTime`，包含起点 50，不包含终点 110。增加主题或时间过滤仍保持文件顺序；需要时间排序或仅索引查询时，见[查询与随机访问](queries.md)。

```csharp
foreach (var message in factory.ReadMessages(new()
{
    Topic = "/events",
    StartTime = 50,
    EndTime = 110
}))
    Console.WriteLine($"{message.Channel.Topic} {message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

## 读取其他记录与摘要

如果已按写入指南添加元数据和附件，将以下代码追加到读取程序。每个 factory 操作执行独立读取。元数据输出 `recording: sample`，附件分别包含 `notes` 和 `abcdef`。

```csharp
foreach (var metadata in factory.ReadMetadata())
    Console.WriteLine($"{metadata.Name}: {metadata.Values["operator"]}");
foreach (var attachment in factory.ReadAttachments())
    Console.WriteLine($"{attachment.Name}: {Encoding.UTF8.GetString(attachment.Data)}");
var summary = factory.GetSummary();
Console.WriteLine(summary is null ? "No summary" : "Summary available");
```

使用 `ReadSchemas()` 和 `ReadChannels()` 枚举声明，包括没有消息的声明。`ReadMetadata()` 与 `ReadAttachments()` 返回独立的数据副本。独立的 `GetSummary()` 结果包含可用声明、统计及索引；没有摘要时返回 null，获取摘要不会校验完整录制。

`OpenRecords()` 将 Chunk 展开为记录，`OpenLinear()` 暴露顶层记录。关注记录结构而非解析后的消息时使用这些入口。`McapRecordAccessor` 提供可寻址原始记录访问；索引读取器支持按索引读取附件/元数据及定位消息。按能力选择读取器，不应假定每种读取器都暴露全部操作。详见[记录契约](api.md#自有记录摘要和原始记录)；应用需要自行驱动输入请求时，使用 [Sans-I/O 读取](sans-io.md)。

## 选择输入与会话

`McapReaderFactory(path)` 保存路径，不打开文件，也不拥有需要释放的输入。每次枚举或打开会话都拥有独立读取器。需要便利的自有结果时使用 `ReadMessages()`；选择缓冲、回调或 lease 时，使用并释放 `OpenMessages()` 返回的会话。

已有 Stream 且由调用方负责清理时，使用 `McapReaderFactory.OpenMessages(stream, leaveOpen: true)`。重复随机访问可使用 `McapIndexedReader`，它拥有复制或映射的输入并需要释放，不仅是索引集合。详见 [Stream 与异步读取](streams-async.md) 与[随机访问](queries.md)。

<a id="选择交付形式"></a>

## 选择如何接收消息

| 需求 | 入口 | 调用方责任 |
| --- | --- | --- |
| 独立可变消息 | `ReadMessages()` | 拥有副本，遵循普通托管生命周期 |
| 复用载荷存储 | `ReadNext(buffer, ...)` | 遇到 `BufferTooSmall` 扩容；复用前处理字节 |
| 立即同步处理 | `VisitMessages` / `ReadNext(visitor)` | 不保留 span，不从回调重入读取器 |
| 保留或转发批次 | `ReadBatchLease` | 所有访问完成后释放 lease，不并发释放 |

需要在读取器前进后继续使用消息时，可通过存储租约（lease）保留底层数据，并在使用后释放租约。访问顺序与交付所有权分别选择。Lease 可能保留整个 Chunk 或映射，读取器释放后仍可保留。[缓冲与消息转发](ownership.md)给出缓冲、回调与 lease 的可运行示例及成本说明。异步消息 lease 见[Stream 与异步读取](streams-async.md)。

缓冲和回调读取分别交付消息头与字节。使用会话的 `GetChannelDescription` 获取该会话遇到的不可变声明。仅在摘要中找到的声明不代表已在顺序扫描中遇到。原始消息、展开记录和顶层记录是不同能力，验证范围也不同。

## 结束与失败处理

`BufferTooSmall` 保留同一条待读记录，提供报告的容量后重试。成功的零字节载荷不是 EOF。`EndOfStream` 表示所选扫描结束，本身不证明完成全文件校验；这一选择见[校验与恢复](validation.md)。

使用后释放打开的会话和枚举器，`foreach` 会释放枚举器。自有结果及独立保留的 lease 继续遵循各自的生命周期。解析、回调和 I/O 失败会终止会话，重复工作应使用新会话，不应重试任意失败。显式清理失败会报告给调用方。异步读取器必须先消费唯一的在途操作，才能启动下一操作或释放。

## 下一步

选择[查询与随机访问](queries.md)、[缓冲与消息转发](ownership.md)，或[校验与恢复](validation.md)。[录制示例](../../samples/Documentation/RecordingExamples.cs)对照 writer 输出检查消息、声明、元数据、附件、摘要、缓冲重试和 EOF；[读取示例](../../samples/Documentation/ReadingExamples.cs)覆盖交付与 Stream API。运行方式见[示例说明](usage.md#运行仓库示例)。
