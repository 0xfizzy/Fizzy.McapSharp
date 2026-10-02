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
| `Complete()` | 完成 MCAP 格式并排空输出缓冲，不请求文件持久化。成功后重复调用无操作；输出保持所有权直至释放或 Stream 转移。 |
| `FlushToDisk()` | 成功 Complete 后，请求路径输出或直接传入的 FileStream 持久化；其他 Stream 抛出 NotSupportedException。 |
| `GetSummary()` | 成功 Complete 后复制上游完成摘要，允许分配。 |
| `Dispose()` | 释放资源，不隐式 Complete。 |

Writer 操作串行化，业务顺序由应用协调。原生操作或 Stream 回调失败使 Writer 终止，需释放并开始新录制；进入原生调用前的参数/状态检查失败本身不使 Writer 终止。输入缓冲可在调用返回后立即复用。

### 格式完成与文件持久化

完成格式后，按需请求操作系统同步文件：

```csharp
using var writer = new McapWriter(path);
// 注册通道并写入消息。
writer.Complete();
writer.FlushToDisk(); // 可选；仅需格式完整时可省略。
```

调用方提供的文件也使用相同的两个动作：

```csharp
using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write);
using var writer = new McapWriter(stream, leaveOpen: true);
// 注册通道并写入消息。
writer.Complete();
writer.FlushToDisk();
```

`Complete()` 完成 Chunk、Summary、Footer 和结束标记，随后普通刷新输出缓冲。写入期间的 `Flush()` 不完成格式；两者均不请求文件持久化。`FlushToDisk()` 对路径输出调用 Rust `File::sync_all()`，对直接传入的 FileStream 调用 `FileStream.Flush(true)`；不解包包装流或其他 Stream。

仅在完成成功后、释放前调用 `FlushToDisk()`，每次有效调用均重新执行同步。完成前调用抛出 InvalidOperationException；释放后抛出 ObjectDisposedException。不支持的 Stream 抛出 NotSupportedException，但不会使 Writer 失败。实际同步失败使 Writer 终止：不能再次完成、同步或获取摘要，但仍可释放及转移 Stream 所有权。Stream 异常保留原始类型和实例；此前已打开的独立摘要游标仍可使用。

路径文件句柄在完成后保留到释放；Stream 仍按 `leaveOpen` 和 `IntoInner()` 管理所有权。释放和所有权转移均不隐式完成或同步。同步成功表示操作系统同步请求成功，不是对硬件、文件系统或目录项持久化的无条件保证。

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

可空开关为 null 时采用上游行为。先应用 `EmitSummaryRecords` 总开关，再应用显式指定的单项开关。`DisableSeeking` 对可定位输出默认 false、不可定位输出默认 true；后者显式指定 false 会失败。省略摘要记录时关闭统计、Chunk/附件/元数据索引和重复声明；Summary offsets 另行关闭。不使用 Chunk 时不会产生 Chunk 压缩和 Chunk 消息索引，不受相关请求值影响。

### 单文件长期录制

单一文件可以保留压缩和完整索引，而不保留全部消息载荷。Writer 内存包含活动 chunk／编解码器存储、声明和文件级索引。上游每完成一个 chunk 都积累一个 ChunkIndex，即使 `EmitChunkIndexes=false` 也是如此；该选项控制输出，不控制积累。`EmitMessageIndexes=false` 省略 chunk 内消息索引，`EmitAttachmentIndexes=false` 和 `EmitMetadataIndexes=false` 则停止保留对应索引。关闭索引会减少索引查询能力。应复用声明，避免持续增加不同 schema／channel。

根据录制时长、吞吐和随机读取延迟选择 `ChunkSize`。较大的 chunk 减少 chunk 索引数量，但可能增加活动存储和单次读取／解压成本；该大小是目标值，不是内存上限。避免不必要的 `Flush()`：它会结束活动 chunk，可能增加索引数量。使用文件或持续排出数据的 Stream，并约束调用方待写队列；持续增长的 MemoryStream 会保留录制内容本身。库不自动分段，也不将索引溢写到磁盘。

`Complete()` 完成格式后释放上游 writer，保留一个共享原生 summary 及输出，不构建持久 JSON summary。`GetSummary()` 按需编码响应并返回独立托管结果；请求整个 summary 仍需要与其内容成比例的响应和结果内存。`OpenSummaryRecords()` 共享原生 summary，不进行完整 JSON 编码；独立游标可以在 writer 释放后继续保留它。上游完成时克隆 summary 的临时成本仍然存在。结束 summary／持久化／Stream 转移操作后应释放 writer；单文件录制仍有文件级索引增长，不是恒定内存操作。

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

`BufferTooSmall` 返回所需 payload 长度和消息头，不修改目标缓冲，也不消费待处理记录。空消息成功返回 Message，长度为零。EOF 可重复读取。错误会终止消息/记录推进；调用方缓冲接口不暴露原生地址；专用借用入口遵循下文生命周期契约。

查询指定完整 `Topic` 或 `Topics` 集合，对 LogTime 使用 `[StartTime, EndTime)`；null 为无边界，起点大于终点抛异常，相等为空区间。查询默认 `LogTime` 顺序，可选 `ReverseLogTime` 或 `File`；同时间消息按文件顺序排列，逆序时也反转。不传查询对象时按官方消息流的文件顺序读取。

可定位时间排序查询在摘要声明和 Chunk 覆盖充分、无 Chunk 外消息时使用官方 `IndexedReader`。非默认线性解析选项使高层查询转为扫描；显式索引读取拒绝这些不支持的选项。直接 Sans-I/O 索引读取不使用扫描排序回退。

### 选择查询路径

| 请求行为 | 执行方式 | 成本 |
| --- | --- | --- |
| 不带查询或选择 File 顺序 | 增量顺序扫描 | 不为排序收集全部匹配消息 |
| 时间排序且索引充分、选项受支持 | 官方 IndexedReader | 可能缓存时间范围重叠的 Chunk |
| 时间排序但索引不足 | 封装层扫描排序回退 | 返回会话前将匹配消息收集到原生内存 |

回退是高层封装增加的能力，不是官方 MessageStream 提供的排序。文件顺序不一定按 LogTime 递增：读到时间 30 后，后续仍可能出现时间 10。缺少充分索引或顺序保证时，必须完成扫描才能确定全局时间顺序，因此增加首条结果延迟，并消耗与选中 payload 和排序数据成比例的内存；不会自动溢写磁盘。

`AllowBufferedSort` 默认 true，非定位源同样允许回退。设为 false 后在收集前抛出 NotSupportedException 拒绝回退，但仍允许索引探测、受支持的索引查询及文件顺序扫描；用 `OpenIndexedMessages()` 明确要求索引可用。`McapQuery.MaxBufferedSortBytes` 限制选中消息的逻辑 payload 长度总和与回退排序描述符数组已分配容量（字节数）之和。即使多个 payload 范围共享同一 backing，也分别计入各自的逻辑长度。这是结果收集额度，不是保留 backing 容量、压缩存储时的临时空间、解析器／编解码器内存或进程 RSS 上限，也不适用于官方索引读取的重叠 Chunk 缓冲。索引查询成功仍不代表全文件已验证。

`GetChannel(id)` 和 `GetSchema(id)` 复制已遇到或从 Summary 加载的描述。文件中途新增声明不会在消息循环创建托管对象，热路径只返回 ID。描述查询与 Summary 操作允许分配。

## 自有记录、摘要和原始记录

`ReadMessages`、`ReadSchemas`、`ReadChannels`、`ReadMetadata`、`ReadAttachments` 提供自有数据便利接口。文件工厂方法各自打开独立会话；会话上的便利方法消费当前游标，Schema/Channel/元数据/附件枚举要求记录会话。消息、Schema、附件的数据是托管数组，释放会话后仍有效；数组可变。消息枚举私下缓存 Channel 描述，对每条结果复制可变 schema 字节和 metadata；修改一条结果不会影响后续消息。

`GetSummary()` 返回统计、Chunk/附件/元数据索引及 Schema/Channel ID 的托管快照，无 Summary 返回 null；完整声明通过会话查询获取。Writer 的完成摘要描述内存中的录制结果，即使输出中关闭了摘要记录也可获取。

`OpenRecords(McapRecordMode.TopLevel)` 返回顶层记录，包括编码后的 Chunk body；`ExpandChunks` 用解压后的内部记录替代 Chunk。`ReadNextRecord(destination, out opcode, out length)` 与消息读取采用相同重试契约，`ReadRecords()` 返回分配的 McapRecord。Body 不含 opcode 和八字节长度前缀，保留未知/私有记录内容。

可寻址源上的 `ReadRecordAt(offset)`、`ReadChunk(index)`、`ReadMessageIndexes(index)` 不消费顺序游标或待处理记录。偏移相对 MCAP 起点。ReadChunk 返回原始 Chunk 记录；OpenRecords(ExpandChunks) 用于顺序解压。消息索引包含 Chunk 内相对偏移。原始随机读取不校验全文件。

分类扫描仍解析、校验并观察所有记录，但只交付目标记录。自有消息／原始记录直接复制到最终托管数组；Schema／Attachment 分类仅复制最终二进制字段，不使用托管 payload 中转缓冲。解析器输入与解压成本仍存在。分别调用文件级分类方法会独立扫描；这些便利方法不提供零分配保证。

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

文件总长度不作为记录或解压后 Chunk 的大小上限。高压缩率 Chunk 解压后的大小可以超过文件大小，但仍需要足够可用内存并受上游解析器限制。


## 完整消息与预准备写入

`WriteMessage(McapMessage)` 调用官方 `Writer::write`，按传入 ID 自动声明。预准备重载接收 `McapPreparedChannel`、匹配的 Header 和 payload Span，避免重复的托管序列化。准备时复制 Schema 与元数据，但不注册声明；描述符保持不可变，便利重载每次读取当前输入。

`McapPreparedOperation` 提供可复用的 Schema、Channel、Metadata 和附件描述符。`WritePrepared` 零托管分配执行并返回适用的注册 ID，payload 单独通过 Span 传入。附件续写、私有记录及 Flush 也提供零分配路径。`Complete()` 显式完成格式，随后可用 `GetSummary()` 获取自有摘要，或用 `OpenSummaryRecords()` 获取缓冲区摘要游标。`IntoInner()` 释放原生状态并交还 Stream，不隐式完成。

## 官方读取器直接适配

`McapBufferReader` 直接适配官方 `LinearReader`、`sans_magic`、`ChunkReader`、`ChunkFlattener`、`RawMessageStream` 和 `MessageStream`，通过 `McapBufferReadMode` 选择；Chunk 模式接收 Chunk 记录体。`ignoreEndMagic` 对应官方切片读取选项。构造时只复制输入，推进时驱动官方 Sans-I/O 解析器，仅保留一条待交付记录；GetChannel 只暴露已遇到的声明，记录错误在到达对应位置时抛出；需要增量输入时使用 Stream 或 Sans-I/O。`ReadNextRecord` 复制记录体，消息模式另有 Header/payload `ReadNext`。RawMessages 模式的 `GetChannel` 保留上游迭代器成功遇到的所有声明，包括没有消息的通道和延迟错误之前的声明。描述和自有对象枚举允许分配。

`McapRecords.Parse` 为所有标准记录返回强类型自有模型，未知记录返回 `McapRecord`。`McapRecordView.Parse` 使用官方 `parse_record` 校验，在调用方托管内存上提供视图；标量属性和 `Fields` 游标可零分配读取 UTF-8、映射、数组和二进制字段。`ToOwned` 显式复制；`ReadFooter` 和 `GetCompressedDataOffset` 直接调用上游辅助函数。

`OpenIndexSnapshot()` 复制可定位源并读取官方摘要，不移动顺序游标；也可用 `new McapIndexSnapshot(bytes)`。快照独立于原会话，原生内存需求与文件大小成比例。提供消息定位、指定 Chunk 消息枚举、消息索引、按索引读取 Metadata/Attachment、Footer、描述和摘要。随机操作使用调用者提供的索引字段，包括长度及通道偏移映射，不按 offset 替换为摘要中的索引。Metadata/Attachment 可在没有摘要时使用调用者提供的索引读取；Chunk 消息和消息索引读取仍需摘要声明。对应上游函数未使用的索引字段不会增加额外校验。调用期间不得修改索引的映射。`OpenChunkReader(index)` 返回独立、惰性、可释放的 McapBufferReader，快照释放后仍然有效。游标共享不可变原生输入和摘要，不再次复制文件。`ReadChunkMessages` 的每个枚举拥有独立游标。随机操作不移动这些游标。缓冲区重载不分配托管对象；Metadata/Attachment 输出记录体，消息索引每项 18 字节小端编码：u16 通道 ID、u64 LogTime、u64 Chunk 内偏移。不足时不修改目标。`OpenSummaryRecords()` 提供摘要字段及声明的缓冲区游标。

### 预编译 Chunk 索引

重复随机访问时，构造一次 `McapPreparedChunkIndex(index)`，使用结束后释放。构造复制索引字段和通道偏移映射，仅编码一次并由官方 Rust 实现解析一次。构造期间不要修改映射；后续修改不会影响 prepared 索引。`SeekMessage`、`OpenChunkReader`、`ReadChunkMessages`、`ReadMessageIndexes` 和 `GetCompressedDataOffset` 的 prepared 重载消除重复索引编码、临时原生 scratch 和索引解析。原有重载仍观察调用方当前字段。

prepared 索引可跨 snapshot 复用。每次操作仍执行原有文件范围及摘要要求；构造不验证某一具体文件。该描述符的调用与释放串行执行。子游标在 prepared 索引和 snapshot 释放后仍可用。消息索引上游 helper、缓存未命中和解压仍可能分配，不能据此保证总原生分配为零。

`McapSansIoReader.CreateLinear/CreateSummary` 及已完成摘要会话的 `CreateIndexed` 提供值类型事件。用 `SupplyInput` 送入字节、`NotifySeeked` 确认定位、`InsertChunkData` 插入索引 Chunk 压缩数据。索引会话支持 `SetRecordLengthLimit`。输出复制到调用方 Span，不返回原生指针。`GetSummary` 返回自有快照，`OpenSummaryRecords` 返回缓冲区游标。上游没有自定义解压器注册入口，因此封装不公开自定义解压接口。内置 Lz4/Zstd 解压由官方 Rust 库处理。

## 异步记录和错误

`McapAsyncReader.ReadNextRecordAsync(Memory<byte>, CancellationToken)` 返回 `ValueTask<McapRecordReadResult>`，由 .NET Stream 异步 I/O 驱动官方线性解析器，对应可选 Tokio 能力，不引入 Tokio runtime。保留待处理记录重试契约。每会话仅一个在途操作；ValueTask 只能消费一次，消费后才能再次读取、转移所有权或释放。取消及 I/O/解析失败使会话终止。Stream 独占、`leaveOpen` 和 `IntoInner` 沿用同步所有权规则。

复用完成源和 continuation，在预热后的实际 I/O 挂起路径也提供 0 B 托管分配。门禁同时计量调用线程和专用 I/O 线程，包含直接 await 循环。完成通知可在 I/O 线程内联恢复调用方，库不强制派发 ThreadPool。第三方 Stream、调用方 await 机制、初始化、错误、扩容及自有结果不在保证内；原生分配不受此保证约束。

`McapException.Kind` 对应全部上游错误变体，`Details` 保留结构化字段；封装层错误使用 `Binding`，原始 Stream 异常仍保留。 原生错误文本有界：消息最多保留 256 个 UTF-8 字节，详情文本值最多保留 128 字节，均在字符边界截断。缩短的文本带 `[truncated]`，对象详情同时设置 `truncated: true`；数值字段保持精确。原生 OS 错误通过 `osCode` 和固定格式消息报告，不分配本地化 OS 文本。 Parse 错误保留根因文本，不序列化 parser 的装饰性回溯。

### 可恢复的 writer 错误

`McapWriterOptions.RecoverableErrors` 是在构造时固定的标志组合。默认启用五种经核验的修改前拒绝：显式 Schema 注册使用 ID 零（`InvalidSchemaIdOnRegistration`）、显式 Schema 冲突（`ConflictingSchemaOnRegistration`）、任一 Channel 注册引用未知 Schema（`UnknownSchemaOnChannelRegistration`）、显式 Channel 冲突（`ConflictingChannelOnRegistration`），以及 header/payload 写入引用未知 Channel（`UnknownChannelOnMessageWrite`）。Prepared Schema/Channel 注册采用相同策略。可以选择任意子集；未知位在创建文件或取得 Stream 所有权前被拒绝。

被拒绝的调用仍抛出 `McapException`。仅当该拒绝发生后 writer 仍可使用时，`CanContinueWriting` 为 true；调用者修正输入后再重试。该值不保证后续操作或并发调用后的状态。自动声明的完整消息写入、附件长度错误、ID 耗尽、I/O、回调、压缩失败和 panic 仍使 writer 终止。库不自动重试。托管参数和状态预检查保持原有行为。

如需所有原生 writer 失败均终止，配置：

```csharp
var options = new McapWriterOptions { RecoverableErrors = McapRecoverableWriterErrors.None };
```

成功消息写入保持零托管分配契约；错误处理不属于该契约。


## 借用、批次与 lease

Owned 结果保持独立：`McapMessage.Data` 仍为 `byte[]` 副本，可变声明仍防御性复制。`ReadNext(McapMessageVisitor)`、`VisitMessages(visitor, maxMessages)` 接收 `in McapMessageHeader` 与 `ReadOnlySpan<byte>`，消除最终交付复制。返回 false 会在当前消息后正常停止。Span 仅在回调期间有效；禁止回调内读取、seek、查询状态或释放同一 reader，可以写入另一个 writer。异常在正常 ABI 返回后重抛。`GetChannelDescription` 返回可共享的不可变声明快照。

`WriteBatch(headers, payloadStorage, ranges)` 一次加锁、一次 ABI 调用，同步消费连续共享载荷。开始前检查数量、范围及全部 Channel。成功返回数量，`McapBatchWriteException.CompletedCount` 不包含可能部分落盘的失败记录。批次不原子；原有安全拒绝配置继续适用，I/O、压缩及推进后的失败终止 writer。返回后可复用输入缓冲。

`WriteBatch(batchLease)` 写入 lease 原有的 header 和 payload；`WriteBatch(batchLease, headers)` 为每条消息替换完整 header，数量必须等于 `batchLease.Count`。先注册目标 schema/channel，需要重映射时提供替换后的 ChannelId；库不推断声明或映射。两个重载均一次 writer 加锁、一次 ABI 调用，直接引用不同存储 owner 的 payload 而不拼接，并在同步调用期间保持 lease 句柄有效。不转移所有权或修改 lease；不得与写入并发释放 lease。null、已释放 lease 和 header 数量不匹配在原生写入前拒绝，不使 writer 失败。Channel 预检查、安全拒绝及已完成前缀遵循上述批次契约。两个重载均有预热后的零托管分配门禁；创建输入 lease 的分配单独计算。

`ReadBatch(headers, ranges, payloadStorage)` 将完整消息写入调用方缓冲，返回数量、使用字节、停止原因和下一条所需容量。空间不足时保留下一条。借用、调用方批量读取和批量写入都有预热后的 Release 零托管分配门禁。

`ReadBatchLease` 返回 `McapMessageBatchLease`，默认最多 256 条、软目标 4 MiB，更大单条独立成批。通过 GetHeader、GetPayload、CopyTo、RetainMessage(index) 访问或保留消息，不产生逐消息载荷数组。批次可引用多个 chunk，不重新拼接。Lease 在 reader 释放后有效；Dispose 幂等，私有 SafeHandle 提供终结兜底。Retain 的消息 lease 单独释放。

Span 使用期间用 using 保持 lease 存活。访问入口拒绝已释放 owner，但已有 Span 无法撤销。禁止访问与 Dispose 并发；可在调用者同步下跨线程转移。所有相关 reader、子游标和 lease 释放前，映射文件必须保持不变。

`ReadBatchLease` 返回批次，EOF 返回 null。异步 `ReadBatchLeaseAsync` 等待 I/O 并支持取消；消费当前 ValueTask 后才能再次读取或释放。取消终止 reader，已交付 lease 仍有效。同一异步 reader 不可混用 record 与 lease 消费；消息 lease 拒绝 EmitChunks。不创建预取队列、溢写文件或文件自动分段。

异步 lease 读取复用完成源和 continuation。每个交付批次允许分配 lease 与 SafeHandle 对象；I/O 挂起无需再创建逐操作 async 状态机。已有消息的批次在 parser 下一次请求输入时返回，即使尚未达到消息数或 payload 目标，避免仅为填满批次而等待额外 I/O。Stream 实现自身仍可能分配。取消释放 parser 资源前必须先消费输入完成结果。

## 绑定层性能与局部限制

性能目标针对绑定层引入的分配与 payload 复制，不限制官方解析器、writer 或 codec 的内部分配。借用回调和 lease 引用稳定存储；缓冲区读取复制到调用方，便利 API 创建独立自有结果。复制构造拥有输入副本；`OpenMapped` 共享映射，Stream 支持增量输入。初始化、描述符、lease 控制对象和错误路径可以分配。

同步与异步输入按 parser 当前完整需求预留空间，I/O 传输大小单独控制，避免短读导致反复扩容。这不是整源预缓冲，也不是总内存上限。一个保留切片可能使整个 chunk 或映射继续存活；限制在途工作时，应同时考虑存储大小与批次数。缓存额度和批次 payload 目标不涵盖全部外部保留字节。

`McapReaderOptions.MaxRandomAccessCacheBytes` 与 `McapIndexSnapshotOptions.MaxRandomAccessCacheBytes` 默认为 0，关闭缓存保留。reader 配置用于其创建的索引快照。设置正值启用按访问顺序淘汰的局部缓存，额度包含保留的 chunk 存储、消息描述符和索引字节，不包含分配器开销、临时解析内存或外部持有的 lease。映射 chunk 按逻辑解压大小保守计入缓存成本。超额条目可以临时加载但不保留；淘汰不影响已交付 lease。

`McapQuery.MaxBufferedSortBytes` 默认为 null。它限制回退排序的逻辑 payload 总长度加描述符容量，不是原生堆上限；共享引用可能保留更大的原生 chunk。超限终止该读取会话，不改变顺序或溢写。`AllowBufferedSort=false` 可完全拒绝回退排序。记录长度限制仍由上游解析器执行。

### 为保留结果选择存储

同步处理使用借用回调，短时异步流水线或转写使用 lease。长期保留少量消息时，只复制需要的 payload 到独立数组，然后释放 lease：

```csharp
byte[] payload;
using (var batch = session.ReadBatchLease(1))
{
    if (batch is null) return;
    payload = new byte[batch.GetPayload(0).Length];
    batch.CopyTo(0, payload);
}
// payload 已拥有独立存储。
```

其他 lease、parser 或缓存仍可能引用原 backing；复制一条切片不保证立即释放整个 chunk。背压应同时考虑不同存储 owner 的数量、大小和保留时长，而不只是消息数量。

回退排序在发布结果前选择存储。按连续遇到的 owned backing 对选中消息分组，在 owner 切换或扫描结束时保留密集组、紧凑化稀疏组。当前内部策略在容量至少为选中 payload 的四倍、且该组 backing 容量可减少至少 256 KiB 时复制。紧凑段以消息边界划分，目标 256 KiB；超大消息独立成段，空 payload 不保留 backing。映射等外部存储继续共享。这些阈值属于实现选择，不是公共配置或性能保证。

紧凑化保持消息顺序、重试与 lease 寿命。选中 payload 只复制一次，期间新旧存储暂时并存；其他 owner 可能延迟实际回收。排序仍先收集结果再交付，其逻辑额度不覆盖瞬时复制、backing 保留放大、parser/codec 内存或进程 RSS。

### 完整 chunk 随机访问

缓存按完整调用方索引语义区分条目，命中不重复解压。未压缩映射 payload 引用映射范围，压缩 payload 共享解压存储。缓冲不足重试保留共享切片。

`SeekMessages(ReadOnlySpan<McapSeekRequest>)` 将同一 chunk 的请求分组，每个不同 chunk 加载一次，并按原请求顺序返回批次（包括重复项）；失败不交付半批。`SeekMessage(preparedIndex, entry, visitor)` 提供借用交付，缓冲区及自有结果 API 在最终交付时复制。`GetCacheStatistics` 报告命中和 chunk 加载次数。

一次性随机访问可保持缓存关闭，避免缓存保留。重复访问相同 chunk 时，配置 `MaxRandomAccessCacheBytes` 覆盖预期工作集，并检查 `GetCacheStatistics()`。同一 chunk 中读取多条消息时，用 `OpenChunkReader` 遍历一次，或用 `SeekMessages` 合并请求。Prepared index 减少重复描述符工作，但不能避免缓存未命中时的解压；复制返回的 payload 也不会缓存源 chunk。

随机读取校验完整目标 chunk，可能比前缀读取更早报告尾部损坏，但不代表全文件校验。补丁边界与证据见[本地补丁](patches.zh-CN.md)。
