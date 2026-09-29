# API、生命周期与数据所有权

[English](api.md) | 简体中文

`Fizzy.McapSharp` 提供 MCAP 文件、Stream、缓冲区和 Sans-I/O 操作，支持 .NET 8、Windows x64 和 glibc Linux x64/ARM64，Linux 构建基线为 Ubuntu 22.04。不支持 macOS、musl 和 32 位进程。消息编码及纳秒时钟语义由应用定义；支持可取消的异步记录读取和时间排序；业务解码由应用实现。参见[官方 API 覆盖表](coverage.zh-CN.md)。

## 写入录制

文件创建语义：`McapWriter(path, options)` 原子地创建新文件，路径已存在时失败，保留已有文件内容。这是路径重载默认的防误覆盖行为，并非 MCAP 格式的要求。

`McapWriter(stream, options, leaveOpen: false)` 接受可写的可寻址或非寻址流，由调用方在打开流时选择创建或截断方式。若需显式覆盖文件，使用 `FileMode.Create` 和 `FileAccess.Write` 打开 `FileStream`，再传给 Writer；打开该流时就会立即截断已有文件。可寻址输出必须位于流末尾，当前位置作为 MCAP 偏移零点；非寻址写入使用原生 Chunk 缓冲。

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
| `Compression`、`ChunkSize`、`UseChunks` | Zstd、1 MiB、true；ChunkSize 遵循上游语义，允许零；null 不按目标大小结束 Chunk。 |
| `Profile`、`Library` | 空 profile；Library 为 null 时使用官方 Rust 库标识。 |
| `EmitSummaryOffsets`、`EmitStatistics` | true，分别控制 Summary offsets 与统计。 |
| `EmitMessageIndexes`、`EmitChunkIndexes`、`EmitAttachmentIndexes`、`EmitMetadataIndexes` | true，分别控制各类索引。 |
| `RepeatChannels`、`RepeatSchemas` | null，采用上游默认 true，控制摘要中的重复声明。 |
| `CalculateChunkCrcs`、`CalculateDataSectionCrc`、`CalculateSummarySectionCrc`、`CalculateAttachmentCrcs` | true，独立控制各区段 CRC。 |
| `CompressionLevel`、`CompressionThreads` | null，采用上游默认和算法支持范围。 |

可空开关为 null 时采用上游行为。先应用 `EmitSummaryRecords` 总开关，再应用显式指定的单项开关。`DisableSeeking` 对可定位输出默认 false、不可定位输出默认 true；后者显式指定 false 会失败。省略摘要记录时关闭统计、Chunk/附件/元数据索引和重复声明；Summary offsets 另行关闭。不使用 Chunk 时不会产生 Chunk 压缩和 Chunk 消息索引，不受相关请求值影响。这些选项不是统一内存配额。

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

查询指定完整 `Topic` 或 `Topics` 集合，对 LogTime 使用 `[StartTime, EndTime)`；null 为无边界，起点大于终点抛异常，相等为空区间。查询默认 `LogTime` 顺序，可选 `ReverseLogTime` 或 `File`；同时间消息按文件顺序排列，逆序时也反转。不传查询对象时按官方消息流的文件顺序读取。

可定位查询在摘要声明和 Chunk 覆盖充分、无 Chunk 外消息时直接使用官方 `IndexedReader`，否则扫描回退。排序回退在返回会话前将选中消息收集到原生内存，非定位源同样适用；文件顺序扫描仍为增量读取。`McapQuery.AllowBufferedSort` 默认 true；设为 false 后，需要全量缓存排序时在收集消息前抛出 `NotSupportedException`，但仍允许索引探测、索引读取及文件顺序扫描。该选项不限制索引读取的重叠 Chunk 缓冲；直接 Sans-I/O 索引读取没有扫描回退。`OpenIndexedMessages` 拒绝缺失/不完整索引。非默认线性解析选项使高层查询回退扫描；显式索引入口拒绝这些不受支持的选项。

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

`McapReaderOptions` 对应官方 Sans-I/O 的 magic、尾随字节、Chunk 输出、CRC 校验/预校验及长度限制。可选 CRC 和尾随字节校验默认关闭。`IsScanComplete` 只表示游标到 EOF；`IsComplete` 要求严格选项下完整顺序扫描成功。用 `McapReaderOptions.Strict` 打开展开记录/消息会话，再调用 `ValidateRemaining()`；非严格会话拒绝该调用，不会声称校验此前消费的数据。索引查询、排序回退和顶层原始扫描不声明全文件校验成功。

`RecoverMessages(accept)` 交付有效消息前缀，遇到第一条损坏记录或 Chunk 停止；必须检查 IsComplete 和 Error，收到消息不代表文件完整。回调异常直接传播，不转换成恢复结果。尚未写入底层流的缓冲内容无法恢复。

初始化、注册和预热后，正常 WriteMessage 与缓冲区 ReadNext 必须严格产生 0 B 托管分配。这是热路径的硬性契约，覆盖跨 Chunk/压缩边界、中途声明、缓冲不足重试和 EOF。读取仍将原生数据复制到调用方内存，并非零拷贝；也不保证进程没有 GC 或 Rust 没有堆分配。调用方扩容、自有记录枚举、描述查询、启动和错误路径不在承诺内。用户 Stream 自身可能分配，本库只承诺桥接层的分配行为。

不支持的平台抛 PlatformNotSupportedException；原生加载错误保留 .NET 类型；原生操作/ABI 失败抛 McapException（继承 IOException）。托管参数/状态检查使用标准异常，已释放对象抛 ObjectDisposedException。参见 [ABI](native.zh-CN.md) 与[分配验收和构建](development.zh-CN.md)。

文件总长度不作为记录或解压后 Chunk 的大小上限。高压缩率 Chunk 解压后的大小可以超过文件大小，但仍受原生内存和上游解析器限制。


## 完整消息与预准备写入

`WriteMessage(McapMessage)` 直接调用官方 `Writer::write`，按传入 ID 自动声明。零分配重载接收 `McapPreparedChannel`、匹配的 Header 和 payload Span。预准备对象保存 Schema 字节与元数据的不可变快照，但不注册声明；便利重载每次读取当前对象内容。

`McapPreparedOperation` 提供 Schema、Channel、Metadata 和附件描述；`WritePrepared` 零托管分配执行并返回注册 ID，payload 单独通过 Span 传入。附件续写、私有记录及 Flush 也提供零分配路径。`Finish()` 完成并返回自有摘要；`Complete()` 后的 `OpenSummaryRecords()` 提供缓冲区摘要游标。`IntoInner()` 释放原生状态并交还 Stream 所有权，不隐式完成文件。

## 官方读取器直接适配

`McapBufferReader` 直接适配官方 `LinearReader`、`sans_magic`、`ChunkReader`、`ChunkFlattener`、`RawMessageStream` 和 `MessageStream`，通过 `McapBufferReadMode` 选择；Chunk 模式接收 Chunk 记录体。`ignoreEndMagic` 对应官方切片读取选项。构造时只复制输入，推进时驱动官方 Sans-I/O 解析器，仅保留一条待交付记录；GetChannel 只暴露已遇到的声明，记录错误在到达对应位置时抛出；需要增量输入时使用 Stream 或 Sans-I/O。`ReadNextRecord` 复制记录体，消息模式另有 Header/payload `ReadNext`。RawMessages 模式的 `GetChannel` 保留上游迭代器成功遇到的所有声明，包括没有消息的通道和延迟错误之前的声明。描述和自有对象枚举允许分配。

`McapRecords.Parse` 为所有标准记录返回强类型自有模型，未知记录返回 `McapRecord`。`McapRecordView.Parse` 使用官方 `parse_record` 校验，在调用方托管内存上提供视图；标量属性和 `Fields` 游标可零分配读取 UTF-8、映射、数组和二进制字段。`ToOwned` 显式复制；`ReadFooter` 和 `GetCompressedDataOffset` 直接调用上游辅助函数。

`OpenIndexSnapshot()` 复制可定位源并读取官方摘要，不移动顺序游标；也可用 `new McapIndexSnapshot(bytes)`。快照独立于原会话，原生内存需求与文件大小成比例。提供消息定位、指定 Chunk 消息枚举、消息索引、按索引读取 Metadata/Attachment、Footer、描述和摘要。随机操作使用调用者提供的索引字段，包括长度及通道偏移映射，不按 offset 替换为摘要中的索引。Metadata/Attachment 可在没有摘要时使用调用者提供的索引读取；Chunk 消息和消息索引读取仍需摘要声明。对应上游函数未使用的索引字段不会增加额外校验。调用期间不得修改索引的映射。`OpenChunkReader(index)` 返回独立、惰性、可释放的 McapBufferReader，快照释放后仍然有效。游标共享不可变原生输入和摘要，不再次复制文件。`OpenChunkMessages` 成功初始化后才替换默认游标；`ReadChunkMessages` 的每个枚举拥有独立游标。随机操作不移动这些游标。缓冲区重载不分配托管对象；Metadata/Attachment 输出记录体，消息索引每项 18 字节小端编码：u16 通道 ID、u64 LogTime、u64 Chunk 内偏移。不足时不修改目标。`OpenSummaryRecords()` 提供摘要字段及声明的缓冲区游标。

`McapSansIoReader.CreateLinear/CreateSummary` 及已完成摘要会话的 `CreateIndexed` 提供值类型事件。用 `SupplyInput` 送入字节、`NotifySeeked` 确认定位、`InsertChunkData` 插入索引 Chunk 压缩数据。索引会话支持 `SetRecordLengthLimit`。输出复制到调用方 Span，不返回原生指针。`GetSummary` 返回自有快照，`OpenSummaryRecords` 返回缓冲区游标。上游没有自定义解压器注册入口，因此封装不公开自定义解压接口。内置 Lz4/Zstd 解压由官方 Rust 库处理。

## 异步记录和错误

`McapAsyncReader.ReadNextRecordAsync(Memory<byte>, CancellationToken)` 返回 `ValueTask<McapRecordReadResult>`，由 .NET Stream 异步 I/O 驱动官方线性解析器，对应可选 Tokio 能力，不引入 Tokio runtime。保留待处理记录重试契约。每会话仅一个在途操作；ValueTask 只能消费一次，消费后才能再次读取、转移所有权或释放。取消及 I/O/解析失败使会话终止。Stream 独占、`leaveOpen` 和 `IntoInner` 沿用同步所有权规则。

复用完成源和 continuation，在预热后的实际 I/O 挂起路径也提供 0 B 托管分配。门禁同时计量调用线程和专用 I/O 线程，包含直接 await 循环。完成通知可在 I/O 线程内联恢复调用方，库不强制派发 ThreadPool。第三方 Stream、调用方 await 机制、初始化、错误、扩容及自有结果不在保证内；原生分配不受此保证约束。

`McapException.Kind` 对应全部上游错误变体，`Details` 保留结构化字段；封装层错误使用 `Binding`，原始 Stream 异常仍保留。

### 可恢复的 writer 错误

`McapWriterOptions.RecoverableErrors` 是在构造时固定的标志组合。默认启用五种经核验的修改前拒绝：显式 Schema 注册使用 ID 零（`InvalidSchemaIdOnRegistration`）、显式 Schema 冲突（`ConflictingSchemaOnRegistration`）、任一 Channel 注册引用未知 Schema（`UnknownSchemaOnChannelRegistration`）、显式 Channel 冲突（`ConflictingChannelOnRegistration`），以及 header/payload 写入引用未知 Channel（`UnknownChannelOnMessageWrite`）。Prepared Schema/Channel 注册采用相同策略。可以选择任意子集；未知位在创建文件或取得 Stream 所有权前被拒绝。

被拒绝的调用仍抛出 `McapException`。仅当该拒绝发生后 writer 仍可使用时，`CanContinueWriting` 为 true；调用者修正输入后再重试。该值不保证后续操作或并发调用后的状态。自动声明的完整消息写入、附件长度错误、ID 耗尽、I/O、回调、压缩失败和 panic 仍使 writer 终止。库不自动重试。托管参数和状态预检查保持原有行为。

如需所有原生 writer 失败均终止，配置：

```csharp
var options = new McapWriterOptions { RecoverableErrors = McapRecoverableWriterErrors.None };
```

成功消息写入保持零托管分配契约；错误处理不属于该契约。


## 原生内存策略

大型录制优先使用增量会话和调用者缓冲。BufferReader 和现有 Snapshot 构造方法仍复制输入；`McapIndexSnapshot.OpenMapped(path, options)` 只读映射文件，不创建完整输入副本。快照和所有子游标释放前必须保持文件不变。Windows 在整个生命周期内保留只读共享限制；Linux 无法阻止并发截断。子游标共享输入，快照释放后仍可使用。

会话、异步和线性 Sans-I/O 通过 `McapReaderOptions.Memory` 配置；摘要读取器通过 `McapSummaryReaderOptions.Memory` 配置；查询通过 `McapQuery.Memory` 配置。会话 reader Memory 非空时，整体优先于 query Memory。Sans-I/O 索引子读取器在 query 未指定时继承摘要策略。BufferReader 新增 `(data, mode, ignoreEndMagic, options)` 重载，复制快照支持 `(data, options)`，会话支持 `OpenIndexSnapshot(options)`。无参数快照方法继承会话策略。快照子游标继承其策略；writer 摘要游标使用默认值。策略在构造时固定。

| `McapMemoryOptions` 属性 | 默认值 | 约束对象 |
| --- | --- | --- |
| `MaxOwnedInputBytes` | null／不限制 | 完整输入副本容量，不限制映射长度 |
| `MaxPendingBufferBytes` | null／不限制 | 重试或摘要编码缓冲容量 |
| `MaxBufferedSortBytes` | null／不限制 | 回退排序 payload 块、描述数组和块容器容量总和 |
| `MaxRetainedBufferBytes` | 8 MiB | 成功交付后每个缓冲保留的容量，也适用于索引 Stream I/O 临时缓冲 |

零是有效值。扩容前按容量而非有效长度检查预算。pending 数据保留至交付，超过保留阈值的缓冲在交付后释放。BufferReader 为支持记录／消息交替重试，保存完整消息体（包含 22 字节 header）；普通消息会话只保存 payload。目标充足时不创建 pending 副本。原始记录接口保留校验后的原始 body，包括官方允许的尾部扩展字节；owned 模型仍遵循官方字段解析语义。摘要游标仅编码当前请求的记录。

这些是分类预算，不是原生／进程总内存限制。官方解析器和压缩器状态、声明、摘要、随机索引辅助分配、托管结果及映射驻留页不在预算内。索引 Stream 临时缓冲纳入统计和保留策略，但不计入 pending 预算；解析器记录长度限制用于约束上游记录／Chunk 大小。排序只移动描述，在块内最后一条消息交付后释放 payload 块。`AllowBufferedSort=false` 仍在收集前拒绝回退。

预算错误使用 `McapException.Kind=Binding`，`Details.resource`、`limit`、`requested` 给出类别和字节数。推进失败会终止读取器。快照构造失败恢复源位置，不消费 pending 消息；底层 Stream 故障可能阻止位置恢复。不自动重试或转存磁盘。

会话、BufferReader、Snapshot、Sans-I/O 和异步读取器的 `GetMemoryStatistics()` 返回无托管分配的值类型，包含当前／峰值受控容量、申请／扩容次数、受测数据路径复制字节数和映射长度。容量覆盖输入副本、交付缓冲、索引 Stream 临时缓冲和排序存储。复制计数覆盖输入复制、解析器供给、pending／arena 存储和调用者缓冲交付，不含冷路径描述序列化及上游内部复制。它不是分配器全局或工作集统计。Snapshot 包含默认游标，不包含独立子游标。每个视图只计一次共享输入，不要累加相关视图。异步统计必须在消费当前操作后查询。不注册 GC 内存压力估算。
