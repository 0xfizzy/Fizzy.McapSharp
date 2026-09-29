# API、生命周期与数据所有权

[English](api.md) | 简体中文

`Fizzy.McapSharp` 提供同步 MCAP 文件及 Stream 操作，支持 .NET 8、Windows x64 和 glibc Linux x64/ARM64，Linux 构建基线为 Ubuntu 22.04。不支持 macOS、musl 和 32 位进程。消息编码及纳秒时钟语义由应用定义；不提供异步、取消、业务数据解码或按时间排序接口。

## 写入录制

`McapWriter(path, options)` 创建新文件，拒绝覆盖。`McapWriter(stream, options, leaveOpen: false)` 接受可写的可寻址或非寻址流。可寻址输出必须位于流末尾，当前位置作为 MCAP 偏移零点；非寻址写入使用原生 Chunk 缓冲。

```csharp
using Fizzy.McapSharp;

using var writer = new McapWriter(path, new() { Compression = McapCompression.Zstd });
var schema = writer.RegisterSchema("sample", "jsonschema", "{}"u8);
var channel = writer.RegisterChannel("/sample", "json", schema);
var header = new McapMessageHeader(channel, Sequence: 0, LogTime: 1000, PublishTime: 900);
writer.WriteMessage(in header, "{\"value\":42}"u8);
writer.Complete();
```

| 操作 | 契约 |
| --- | --- |
| `RegisterSchema(name, encoding, data)` | 返回原生分配的 ushort ID；首参数为 id 的重载指定非零 ID，等价内容可能去重。 |
| `RegisterChannel(topic, messageEncoding, schemaId, metadata)` | 返回 Channel ID；首参数为 id 的重载指定 ID，允许零。Schema ID 零表示无 Schema；同一 ID 的冲突内容会失败。 |
| `WriteMessage(in header, data)` | 使用消息头和 payload span 写入已注册 Channel，同步消费输入，不创建托管 payload 副本。 |
| `WriteMetadata(name, metadata)` | 写入字符串键值对。 |
| `WriteAttachment(name, mediaType, logTime, createTime, data)` | 一次写入附件。 |
| `StartAttachment(..., length)`、`WriteAttachmentBytes(data)`、`FinishAttachment()` | 按准确声明长度分段写附件；结束前拒绝其他写入操作，长度不符使 Writer 终止失败。 |
| `WritePrivateRecord(opcode, data, includeInChunks)` | 接受 0x80–0xFF 的私有操作码，可选择写入 Chunk。 |
| `Flush()` | 调用上游 flush，不完成 MCAP 尾部，也不保证持久化。 |
| `Complete()` | 完成 MCAP；文件额外执行 sync_all，Stream 执行 Flush，但不承诺物理持久化。成功后重复调用无操作。 |
| `GetSummary()` | 成功 Complete 后复制上游完成摘要，允许分配。 |
| `Dispose()` | 释放资源，不隐式 Complete。 |

Writer 操作串行化，业务顺序由应用协调。原生操作或 Stream 回调失败使 Writer 终止，需释放并开始新录制；进入原生调用前的参数/状态检查失败本身不使 Writer 终止。输入缓冲可在调用返回后立即复用。

### 写入选项

| 选项 | 默认值与含义 |
| --- | --- |
| `Compression`、`ChunkSize`、`UseChunks` | None、4 MiB、true；ChunkSize 为正数或 null，null 不按目标大小结束 Chunk。 |
| `Profile`、`Library` | 空 profile；Library 为 null 时使用本库原生标识。 |
| `EmitSummaryOffsets`、`EmitStatistics` | true，分别控制 Summary offsets 与统计。 |
| `EmitMessageIndexes`、`EmitChunkIndexes`、`EmitAttachmentIndexes`、`EmitMetadataIndexes` | true，分别控制各类索引。 |
| `RepeatChannels`、`RepeatSchemas` | null，采用上游默认 true，控制摘要中的重复声明。 |
| `CalculateChunkCrcs`、`CalculateDataSectionCrc`、`CalculateSummarySectionCrc`、`CalculateAttachmentCrcs` | true，独立控制各区段 CRC。 |
| `CompressionLevel`、`CompressionThreads` | null，采用上游默认和算法支持范围。 |

可空开关为 null 时采用上游行为。Summary 内容由各记录开关控制，没有存在覆盖优先级的总开关。省略摘要记录时关闭统计、Chunk/附件/元数据索引和重复声明；Summary offsets 另行关闭。不使用 Chunk 时不会产生 Chunk 压缩和 Chunk 消息索引，不受相关请求值影响。这些选项不是统一内存配额。

## 使用可复用缓冲读取消息

文件 `McapReader` 是工厂，构造时不打开文件；每次 `OpenMessages` 或 `OpenRecords` 创建独立的可释放 `McapReadSession` 和原生句柄。会话必须释放，建议使用 using。

```csharp
using var session = new McapReader(path).OpenMessages(new() { Topic = "/sample" });
byte[] buffer = new byte[64 * 1024];
while (true)
{
    var status = session.ReadNext(buffer, out var header, out var length);
    if (status == McapReadStatus.EndOfStream) break;
    if (status == McapReadStatus.BufferTooSmall)
    {
        buffer = new byte[checked((int)length)]; // 调用方扩容，不属于零分配承诺
        continue;
    }
    // 在复用缓冲前处理 buffer.AsSpan(0, checked((int)length))。
}
```

`BufferTooSmall` 返回所需 payload 长度和消息头，不修改目标缓冲，也不消费待处理记录。空消息成功返回 Message，长度为零。EOF 可重复读取。错误会终止消息/记录推进；公共接口不暴露原生地址或借用 span。

查询按完整 Topic 匹配，对 LogTime 使用 `[StartTime, EndTime)`；null 表示无边界，起点大于终点抛异常，相等表示空区间。结果保留文件/Chunk 顺序。可寻址查询在摘要声明及 Chunk 覆盖充分、且没有 Chunk 外消息时按索引选择重叠 Chunk，否则顺序扫描；非寻址查询始终顺序扫描。

`GetChannel(id)` 和 `GetSchema(id)` 复制已遇到或从 Summary 加载的描述。文件中途新增声明不会在消息循环创建托管对象，热路径只返回 ID。描述查询与 Summary 操作允许分配。

## 自有记录、摘要和原始记录

`ReadMessages`、`ReadSchemas`、`ReadChannels`、`ReadMetadata`、`ReadAttachments` 提供自有数据便利接口。文件工厂方法各自打开独立会话；会话上的便利方法消费当前游标，Schema/Channel/元数据/附件枚举要求记录会话。消息、Schema、附件的数据是托管数组，释放会话后仍有效；数组可变。消息枚举在自身范围内缓存 Channel 描述，因此同一枚举的消息可能共享 Channel/Schema 对象。

`GetSummary()` 返回统计、Chunk/附件/元数据索引及 Schema/Channel ID 的托管快照，无 Summary 返回 null；完整声明通过会话查询获取。Writer 的完成摘要描述内存中的录制结果，即使输出中关闭了摘要记录也可获取。

`OpenRecords(McapRecordMode.TopLevel)` 返回顶层记录，包括编码后的 Chunk body；`ExpandChunks` 用解压后的内部记录替代 Chunk。`ReadNextRecord(destination, out opcode, out length)` 与消息读取采用相同重试契约，`ReadRecords()` 返回分配的 McapRecord。Body 不含 opcode 和八字节长度前缀，保留未知/私有记录内容。

可寻址源上的 `ReadRecordAt(offset)`、`ReadChunk(index)`、`ReadMessageIndexes(index)` 不消费顺序游标或待处理记录。偏移相对 MCAP 起点。ReadChunk 返回原始 Chunk 记录；OpenRecords(ExpandChunks) 用于顺序解压。消息索引包含 Chunk 内相对偏移。原始随机读取不校验全文件。

## Stream 会话与所有权

使用 `McapReader.OpenMessages(stream, query, leaveOpen)` 或 `OpenRecords(stream, mode, leaveOpen)` 读取 Stream。当前位置作为 MCAP 偏移零点，从此位置到流末尾必须是一份完整 MCAP。会话增量读取，不完整复制流，不使用临时文件。

一个 Stream 同时只能由一个 MCAP 会话占用。会话活动期间，调用方不得自行定位、截断、读写或释放 Stream。回调在发起线程同步执行，拒绝回调重入同一 Writer/会话；回调异常在退出原生边界后传播。leaveOpen 默认 false，释放会话时关闭流，true 时保留。

非寻址流不支持随机读取或提前获取 Summary，相应调用抛 NotSupportedException。顺序扫描至 EOF 后，GetSummary 返回实际读到的摘要，没有摘要则返回 null。重新开始或切换读取模式需要先释放会话，再由调用方重新打开/定位数据源。

映射文件在读取、校验和恢复期间必须保持不变。Windows 在映射期间拒绝普通写入/删除打开，但无法排除之前已有的可写映射；Linux 不强制互斥。并发截断映射文件可能终止进程。

## 校验、恢复和分配承诺

`McapReader.Validate()` 扫描全文件，检查记录解析、存在的 Chunk/Attachment/Data/Summary CRC、记录边界和结束 magic，返回扫描记录数而非消息数。CRC 为零表示未提供校验和；不检查业务 Schema 语义。

展开 Chunk 的顺序会话在推进时校验。`ValidateRemaining()` 将此类会话读至 EOF，返回累计扫描数。只有完整顺序扫描成功后 IsComplete 才为 true；索引查询、顶层原始扫描、提前释放及失败时为 false。查询成功不能代替完整校验。非寻址校验和恢复只扫描一遍。

`RecoverMessages(accept)` 交付有效消息前缀，遇到第一条损坏记录或 Chunk 停止；必须检查 IsComplete 和 Error，收到消息不代表文件完整。回调异常直接传播，不转换成恢复结果。尚未写入底层流的缓冲内容无法恢复。

初始化、注册和预热后，正常 WriteMessage 与缓冲区 ReadNext 必须严格产生 0 B 托管分配。这是热路径的硬性契约，覆盖跨 Chunk/压缩边界、中途声明、缓冲不足重试和 EOF。读取仍将原生数据复制到调用方内存，并非零拷贝；也不保证进程没有 GC 或 Rust 没有堆分配。调用方扩容、自有记录枚举、描述查询、启动和错误路径不在承诺内。用户 Stream 自身可能分配，本库只承诺桥接层的分配行为。

不支持的平台抛 PlatformNotSupportedException；原生加载错误保留 .NET 类型；原生操作/ABI 失败抛 McapException（继承 IOException）。托管参数/状态检查使用标准异常，已释放对象抛 ObjectDisposedException。参见 [ABI](native.zh-CN.md) 与[分配验收和构建](development.zh-CN.md)。

文件总长度不作为记录或解压后 Chunk 的大小上限。高压缩率 Chunk 解压后的大小可以超过文件大小，但仍受原生内存和上游解析器限制。
