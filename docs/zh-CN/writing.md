# 写入文件

<a id="写入录制"></a>

[English](../en/writing.md) | 简体中文

先用下面的程序保存多个主题的消息，再按需添加元数据、附件或选择其他写入方式。项目准备见[入门教程](usage.md)，生成的文件可交给[读取消息](reading.md)。

## 写入多主题文件

在入门教程的项目中运行下面的 C# 12 程序。保留打印的路径供读取指南使用，不再需要时删除文件。

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"recording-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var schema = writer.RegisterSchema("value", "jsonschema", "{\"type\":\"object\"}"u8);
    var values = writer.RegisterChannel("/values", "json", schema);
    var events = writer.RegisterChannel("/events", "text/plain");
    writer.WriteToKnownChannel(new(values, 0, 100, 90), "{\"value\":1}"u8);
    writer.WriteToKnownChannel(new(events, 0, 50, 50), "started"u8);
    writer.WriteToKnownChannel(new(events, 1, 110, 110), []);
    writer.Complete();
}
Console.WriteLine(path);
```

消息刻意按日志时间 100、50、110 写入，写入不会排序。最后一条载荷为空，仍然是一条消息。调用返回前会消费完载荷输入，随后即可复用调用方缓冲。

目标路径必须不存在。调用 `Complete()` 完成文件；退出 `using` 只释放资源，不会自动完成文件。消息头中的日志时间和发布时间均为纳秒，起点由应用约定。

## 理解声明与消息

Schema 描述载荷格式；Channel 将主题、消息编码与可选 Schema 及元数据关联。先注册一次 Schema，再使用其 ID 注册相关通道。Schema ID 为零表示没有 Schema。消息头指定通道，并携带序号、日志时间和发布时间；载荷单独传入。

应用定义载荷编码、序号策略及纳秒时间戳的时钟基准。库不负责序列化业务对象，也不按 Schema 校验载荷。ID 属于具体录制：转发到其他文件时，应注册目标声明并按需映射 ID。

## 添加元数据与附件

将以下代码插入上例的 `writer.Complete()` 之前，再运行程序。读取指南的元数据和附件示例使用这些内容。

```csharp
writer.WriteMetadata("recording", new Dictionary<string, string> { ["operator"] = "sample" });
writer.WriteAttachment("notes.txt", "text/plain", 100, 90, "notes"u8);
writer.StartAttachment("parts.txt", "text/plain", 100, 90, 6);
writer.WriteAttachmentBytes("abc"u8);
writer.WriteAttachmentBytes("def"u8);
writer.FinishAttachment();
```

Metadata 保存带名称的字符串键值表。附件保存带名称、媒体类型和时间戳的字节。分段写入无需在调用方缓冲中组装整个载荷，但必须预先给出精确总长度。`StartAttachment` 与 `FinishAttachment` 之间只提供该附件的字节；附件开启期间其他 writer 操作会被拒绝，长度不匹配会终止 writer。

私有记录写入仅支持 0x80–0xFF 操作码。能够读取任意原始记录不代表提供通用原始记录 writer，详见[写入契约](api.md#写入录制)。

<a id="完成与失败处理"></a>

## 完成、持久化与释放

| 操作 | 用途 |
| --- | --- |
| 写入期间 `Flush()` | 刷新 writer 输出并结束当前 Chunk，不完成文件 |
| `Complete()` | 完成 MCAP 结构并执行普通输出刷新，必须显式调用 |
| 完成后 `FlushToDisk()` | 为路径输出或直接传入的 FileStream 请求文件同步 |
| `Dispose()` | 释放资源，不完成录制 |

成功完成后，`GetSummary()` 返回独立副本；`OpenSummaryRecords()` 提供保留摘要的游标。摘要、同步及 Stream 转移操作结束后释放 writer。`IntoInner()` 转移 Stream，不隐式完成或同步文件。同步请求成功并不构成无条件的硬件持久化保证。

## 选择输出

`new McapWriter(path)` 创建新文件，拒绝已有路径且不修改原文件。需要截断或其他输出目标时，自行打开可写 Stream 后交给 writer。可寻址输出必须位于末尾，该位置成为 MCAP 偏移零点；不可寻址输出使用原生 Chunk 缓冲。

除非指定 `leaveOpen: true`，writer 拥有传入的 Stream。完整所有权示例见 [Stream 与异步读取](streams-async.md)。Writer 操作同步执行并串行化；多个生产者之间的消息顺序由应用决定。

## 选择写入形式

| 应用已有的数据 | 写入形式 | 准备与成本 |
| --- | --- | --- |
| 已注册通道和可复用字节 | `WriteToKnownChannel(header, span)` | 通道注册一次；预热后的绑定消息写入为托管 0 B 分配 |
| 包含完整声明的自有消息 | `WriteMessage(message)` | 按给定 ID 自动声明；读取当前输入描述，可能分配 |
| 重复写入完整消息 | `WriteMessage(prepared, header, span)` | 对通道做一次快照；写入时声明；消息头 ID 必须匹配 |
| 消息头及已位于同一存储区的载荷 | `WriteBatch(headers, storage, ranges)` | 先注册通道；不应仅为批量写入而拼接载荷 |
| 保留的读取批次 | `WriteBatch(lease, headers)` | 注册目标声明，按需替换 ID；整个调用期间保留 lease |

自动声明使用完整通道描述，无需另外调用注册：

```csharp
using var output = new MemoryStream();
var schema = new McapSchema(1, "value", "jsonschema", "{}"u8.ToArray());
var channel = new McapChannel(7, "/values", "json", schema, new Dictionary<string, string>());
using (var writer = new McapWriter(output, leaveOpen: true))
{
    writer.WriteMessage(new McapMessage(channel, 0, 100, 90, "{}"u8.ToArray()));
    using var prepared = new McapPreparedChannel(channel);
    writer.WriteMessage(prepared, new(channel.Id, 1, 200, 190), "{}"u8);
    writer.Complete();
}
```

这是独立示例。准备操作对 Schema 字节和元数据做快照，不注册声明。最后一次使用后释放 prepared 描述。显式注册、元数据和附件也有对应的类型化 prepared 描述，详见 [prepared 契约](api.md#完整消息与预准备写入)。批次操作不具备原子性，不应重放整个失败批次，参见[缓冲与消息转发](ownership.md)。

## 处理失败

默认允许五类经过审计、发生在变更前的原生拒绝继续写入：注册零 Schema ID、注册冲突 Schema、通道注册引用未知 Schema、注册冲突 Channel，以及消息头/载荷写入引用未知 Channel。它们仍抛出 `McapException`；检查 `CanContinueWriting` 并纠正该次输入后再继续。完整消息的自动声明不在白名单内。设置 `SafeRejections = McapWriterSafeRejections.None` 可使所有原生 writer 失败均为终止性失败。

I/O、压缩、回调及其他未经审计的原生失败会终止 writer。释放它并开始新录制，不要重试任意失败操作。进入原生写入前的托管参数/状态检查本身不会终止 writer。显式清理也可能失败，必须让调用方观察到。完整规则见[安全拒绝](api.md#writer-安全拒绝)与 [Stream 所有权](api.md#stream-会话与所有权)。

## 下一步

选择[压缩、分块与索引](writer-options.md)、[读取消息](reading.md)，或[缓冲与消息转发](ownership.md)。[写入示例](../../samples/Documentation/WritingExamples.cs)与[录制往返示例](../../samples/Documentation/RecordingExamples.cs)执行并检查上述操作，运行方式见[示例说明](usage.md#运行仓库示例)。
