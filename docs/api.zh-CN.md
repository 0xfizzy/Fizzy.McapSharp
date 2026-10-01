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

可空开关为 null 时采用上游行为。先应用 `EmitSummaryRecords` 总开关，再应用显式指定的单项开关。`DisableSeeking` 对可定位输出默认 false、不可定位输出默认 true；后者显式指定 false 会失败。省略摘要记录时关闭统计、Chunk/附件/元数据索引和重复声明；Summary offsets 另行关闭。不使用 Chunk 时不会产生 Chunk 压缩和 Chunk 消息索引，不受相关请求值影响。Memory 选项指定有限共享存储预算，包含 codec 堆工作区；未完成路径及受控堆之外的资源见下文计费边界。

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

`AllowBufferedSort` 默认 true，非定位源同样允许回退。设为 false 后在收集前抛出 NotSupportedException 拒绝回退，但仍允许索引探测、受支持的索引查询及文件顺序扫描；用 `OpenIndexedMessages()` 明确要求索引可用。`McapQuery.Memory.MaxBufferedSortBytes` 限制回退受控分配，不限制全部原生内存，也不限制官方索引读取的重叠 Chunk 缓冲。索引查询成功仍不代表全文件已验证。

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

文件总长度不作为记录或解压后 Chunk 的大小上限。高压缩率 Chunk 解压后的大小可以超过文件大小，但仍受原生内存和上游解析器限制。


## 完整消息与预准备写入

`WriteMessage(McapMessage)` 通过借用声明字段复用官方 `Writer::write` 逻辑，按传入 ID 自动声明。零分配重载接收 `McapPreparedChannel`、匹配的 Header 和 payload Span。预准备对象保存 Schema 字节与元数据的不可变快照，但不注册声明。`McapPreparedChannel(channel, budget)` 使用指定资源域，原构造签名使用独立默认域。计费 JSON 字段、Schema 字节及私有根对象跨 writer 调用时保持原域，释放描述符时归还；便利重载每次读取当前对象内容。

`McapPreparedOperation` 提供 Schema、Channel、Metadata 和附件描述。预算重载接受共享的 `McapMemoryBudget`：Schema 和 Channel 将其放在首参数，Metadata 和附件工厂将其放在末参数。旧签名创建独立默认域；跨 writer 复用时，已存在存储保留原域。描述根对象、schema 载荷及解析后的 JSON 控制存储已精确计费。JSON 字符串使用计费存储，对象字段使用分页平衡索引。普通 writer 控制调用将临时 JSON 存储计入 writer 域，返回时释放；输入单块或总预算拒绝会终止 writer。Metadata 和 Channel 写入直接遍历字段，不再创建临时 map。摘要嵌套物化仍是[分配清单](memory-accounting.zh-CN.md)中的未完成项。`WritePrepared` 零托管分配执行并返回注册 ID，payload 单独通过 Span 传入。附件续写、私有记录及 Flush 也提供零分配路径。`Complete()` 显式完成写入，随后通过 `GetSummary()` 获取自有摘要；`Complete()` 后的 `OpenSummaryRecords()` 提供缓冲区摘要游标。`IntoInner()` 释放原生状态并交还 Stream 所有权，不隐式完成文件。

## 官方读取器直接适配

`McapBufferReader` 直接适配官方 `LinearReader`、`sans_magic`、`ChunkReader`、`ChunkFlattener`、`RawMessageStream` 和 `MessageStream`，通过 `McapBufferReadMode` 选择；Chunk 模式接收 Chunk 记录体。`ignoreEndMagic` 对应官方切片读取选项。构造时只复制输入，推进时驱动官方 Sans-I/O 解析器，仅保留一条待交付记录；GetChannel 只暴露已遇到的声明，记录错误在到达对应位置时抛出；需要增量输入时使用 Stream 或 Sans-I/O。`ReadNextRecord` 复制记录体，消息模式另有 Header/payload `ReadNext`。RawMessages 模式的 `GetChannel` 保留上游迭代器成功遇到的所有声明，包括没有消息的通道和延迟错误之前的声明。描述和自有对象枚举允许分配。

`McapRecords.Parse` 为所有标准记录返回强类型自有模型，未知记录返回 `McapRecord`。`McapRecordView.Parse` 使用官方 `parse_record` 校验，在调用方托管内存上提供视图；标量属性和 `Fields` 游标可零分配读取 UTF-8、映射、数组和二进制字段。`ToOwned` 显式复制；`ReadFooter` 和 `GetCompressedDataOffset` 直接调用上游辅助函数。

`OpenIndexSnapshot()` 复制可定位源并读取官方摘要，不移动顺序游标；也可用 `new McapIndexSnapshot(bytes)`。快照独立于原会话，原生内存需求与文件大小成比例。提供消息定位、指定 Chunk 消息枚举、消息索引、按索引读取 Metadata/Attachment、Footer、描述和摘要。随机操作使用调用者提供的索引字段，包括长度及通道偏移映射，不按 offset 替换为摘要中的索引。Metadata/Attachment 可在没有摘要时使用调用者提供的索引读取；Chunk 消息和消息索引读取仍需摘要声明。对应上游函数未使用的索引字段不会增加额外校验。调用期间不得修改索引的映射。`OpenChunkReader(index)` 返回独立、惰性、可释放的 McapBufferReader，快照释放后仍然有效。游标共享不可变原生输入和摘要，不再次复制文件。`ReadChunkMessages` 的每个枚举拥有独立游标。随机操作不移动这些游标。缓冲区重载不分配托管对象；Metadata/Attachment 输出记录体，消息索引每项 18 字节小端编码：u16 通道 ID、u64 LogTime、u64 Chunk 内偏移。不足时不修改目标。`OpenSummaryRecords()` 提供摘要字段及声明的缓冲区游标。

### 预编译 Chunk 索引

重复随机访问时，构造一次 `McapPreparedChunkIndex(index)`，使用结束后释放。构造复制索引字段和通道偏移映射，仅编码一次并由官方 Rust 实现解析一次。构造期间不要修改映射；后续修改不会影响 prepared 索引。`SeekMessage`、`OpenChunkReader`、`ReadChunkMessages`、`ReadMessageIndexes` 和 `GetCompressedDataOffset` 的 prepared 重载消除重复索引编码、临时原生 scratch 和索引解析。原有重载仍观察调用方当前字段。

prepared 索引可跨 snapshot 复用。每次操作仍执行原有文件范围及摘要要求；构造不验证某一具体文件。该描述符的调用与释放串行执行。子游标在 prepared 索引和 snapshot 释放后仍可用。prepared 存储拥有独立默认有限域，也可显式共享预算；不计入逐 snapshot 统计。消息索引上游 helper、缓存未命中和解压仍可能分配，不能据此保证总原生分配为零。

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

`WriteBatch(headers, payloadStorage, ranges)` 一次加锁、一次 ABI 调用，同步消费连续共享载荷。开始前检查数量、范围及全部 Channel。成功返回数量，`McapBatchWriteException.CompletedCount` 不包含可能部分落盘的失败记录。批次不原子；原有安全拒绝配置继续适用，I/O、压缩及推进后的预算失败终止 writer。返回后可复用输入缓冲。

`ReadBatch(headers, ranges, payloadStorage)` 将完整消息写入调用方缓冲，返回数量、使用字节、停止原因和下一条所需容量。空间不足时保留下一条。借用、调用方批量读取和批量写入都有预热后的 Release 零托管分配门禁。

`ReadBatchLease` / `TryReadBatchLease` 返回 `McapMessageBatchLease`，默认最多 256 条、软目标 4 MiB，更大单条独立成批。通过 GetHeader、GetPayload、CopyTo、RetainMessage(index) 访问或保留消息，不产生逐消息载荷数组。批次可引用多个 chunk，不重新拼接。Lease 在 reader 释放后有效；Dispose 幂等，私有 SafeHandle 提供终结兜底。Retain 的消息 lease 单独释放。

Span 使用期间用 using 保持 lease 存活。访问入口拒绝已释放 owner，但已有 Span 无法撤销。禁止访问与 Dispose 并发；可在调用者同步下跨线程转移。所有相关 reader、子游标和 lease 释放前，映射文件必须保持不变。

活跃 lease 导致临时不足时 TryReadBatchLease 返回 BudgetUnavailable，ReadBatchLease 抛 McapMemoryBudgetUnavailableException；释放后重试。永久尺寸超限报错。McapAsyncReader.ReadBatchLeaseAsync 可取消等待容量，等待不持有 reader 锁。消费当前 ValueTask 后才能再次读取或释放。取消终止 reader，已交付 lease 有效。同一异步 reader 不可混用 record 与 lease 消费，消息 lease 拒绝 EmitChunks。不创建无界队列，不预取、溢写或自动分段。

## 原生内存策略

McapMemoryOptions.Budget 可共享 McapMemoryBudget，否则独立 reader/writer 各建资源域。快照子游标继承域，writer options 提供 Memory，prepared Chunk index 构造可传预算。默认 **256 MiB 已计费容量、64 MiB 单块、域内 64 MiB 空闲池保留**。有限默认值属于行为变化；需要时显式提高。复制构造保留隔离语义，超大输入拒绝；大文件使用 OpenMapped 或增量 Stream。块包括记录头，因此恰好等于块上限的 payload 可能需要更大的块。

存活的 `McapMemoryBudget` 会保留域根与句柄两部分已计费的 Scratch 控制存储，因此仅释放 reader／writer 不会使仍存活预算对象的域占用归零。总预算小于必要控制存储时，构造立即拒绝。最后强引用清空空闲存储并打破弱注册环，域根账本保留至最后弱引用释放。域查找与通知链接共用句柄分配；通知登记和分发不分配原生堆。

调用方缓冲区不足时，重试在可用情况下保留已有不可变存储范围，不再将 payload 复制到第二个 pending 缓冲。`MaxPendingBufferBytes` 仍限制保留的逻辑长度：消息会话按 payload 长度，buffer reader 的 record/message 重试及异步记录读取按完整 record body 长度检查。重复缓冲不足调用不写目标、不消费 pending 记录；buffer reader 的 pending Message 记录可切换 record/message 交付方式。借用与 lease 复用存储所有者，owned 与调用方缓冲交付只复制一次。小 pending 范围可能保留较大的输入或解压块。`MaxRetainedBufferBytes` 只控制可复用自有交付缓冲，不控制共享所有者；`MaxBlockBytes` 限制实际存储分配，不对映射切片新增分配限制。

附加限制 MaxOwnedInputBytes、MaxPendingBufferBytes、MaxScratchBufferBytes、MaxBufferedSortBytes 默认 null，仅表示没有附加限制，不能绕过域上限。MaxRetainedBufferBytes 保持逐交付缓冲 8 MiB。MaxRandomAccessCacheBytes 默认零关闭，可设为 64 MiB 等有限值启用。

输入／解压块、pending／scratch、lease／排序描述符、prepared Chunk 索引及保留的摘要／writer 元数据在扩容前计费。元数据采用保守预留。同一 payload 同时被多个 lease 和缓存引用只计一次，空闲池仍计费。排序引用共享载荷，超限报错，不改变顺序或溢写。长期 writer 保留索引，可提高有限预算、显式关闭不需要的索引，或由消费者分段录制。

Zstd/Lz4 编码及解码上下文使用固定 codec 版本的自定义分配接口。codec 堆请求、分配头及输出缓冲在分配前预留容量；拒绝通过正常错误返回，不跨 C 展开。codec 工作区受总预算限制，不套用载荷块上限。CompressionThreads 保持原有行为。推进后的 codec 失败是终止失败，不作为可重试预算压力。解码分配失败在 McapException.Details 中提供 resource、limit、domainLimit、requested、current、phase，并附带 failureKind（permanent、temporary、system、overflow 或 codec-panic）和 terminal。即使容量分类为 temporary，terminal=true 仍表示解码器不能继续使用。`limit` 是触发拒绝的适用上限，`domainLimit` 始终是资源域总上限；单块和局部资源限额可能更低。绑定层资源检查同样报告两者及检查时的域占用。

这些诊断采用固定原生存储；损坏帧诊断使用 codec 静态错误名。

GetDetailedStatistics() 返回无托管分配的固定值类型快照：九类资源（输入、解压、writer、codec 编码／解码、索引、描述符、声明、scratch）的当前／峰值计费、实际存活及未使用预留，以及分配活动、受测复制／codec 流量、解压开始／完成和缓存事件。CurrentBytes、PeakBytes、IdleBytes 来自同一快照。各分类 CurrentBytes 之和等于域 CurrentBytes；不可累加分类峰值。ActiveLeasePayloadBytes 与 CachedPayloadBytes 在各自拥有权维度中对共享堆载荷块去重，与资源分类及彼此重叠，不含映射载荷页和描述符页。

公开预算统计是固定值类型快照。其构造与解构签名独立于私有 ABI，公开类型不承诺 native 内存布局。读取及转换统计快照不产生托管分配。

`ReallocationCount` 记录已接入路径成功替换存储的扩容次数。`ImmediatelyReclaimableBytes` 包含已登记的空闲存储及仅由缓存持有的存储；`MappedLogicalBytes` 按映射所有者去重，与堆容量独立。`Flow.ReclaimedBytes` 当前记录空闲池物理释放；直接缓存释放归因仍待完成。这些字段不表示[分配清单](memory-accounting.zh-CN.md)中的未完成路径已实现完整计费。

GetStatistics 保留五字段接口。AllocationCount 包含预留增长次数；StorageCopyBytes 不含最终交付复制。详细统计 AllocationCount 计受测已提交分配。Flow 在已接入的操作处累计，不等于完整分配器轨迹。GetMemoryStatistics 仍为逐句柄视图，不累加关联句柄；不注册 GC 内存压力估算。

消息和 chunk 索引、长期 writer 索引列表、不可变摘要索引列表、批次描述符、缓存消息范围和排序描述符采用有界页面及分页目录。共享摘要保留页面，不克隆完整索引数组。writer 消息索引直接从页面序列化。

**计费边界：**这不是进程工作集或完整分配器硬上限。操作系统工作线程栈／运行时资源和分配器碎片在受控堆之外。剩余未接入控制块、冷路径 JSON／记录解析临时对象及错误载荷属于未完成计费工作，不是获准排除项；当前范围见分配清单。可回收和映射长度字段覆盖已登记存储，不代表所有未关闭路径均已计费。原生 Rust 分配探针包含经过新 codec 回调的分配，进程级计数包含工作线程；线程局部计数不含其他线程。未经过这些回调的直接外部分配仍不在探针范围内。

### 完整 chunk 随机访问

可选多 chunk LRU 按字节限制，作用域为快照源，键包含完整调用方索引语义。条目保存不可变存储、消息 offset 和完整 chunk 校验。未压缩映射 payload 引用映射范围，压缩 payload 共享最终解压块，命中不解压。超过缓存额度的条目在域预算充足时可临时加载，不无界回退。淘汰仅移除缓存引用，活跃 lease 继续计费。域预算压力先回收空闲存储，再按访问顺序遍历不同 reader 的注册缓存；忙碌条目跳过，回收及析构不持有预算锁。逐缓存限额继续生效。

SeekMessages(ReadOnlySpan<McapSeekRequest>) 对 prepared 索引分组，每个不同 chunk 加载一次，按原顺序返回一个批次 lease，保留重复请求；失败不返回半批。SeekMessage(preparedIndex, entry, visitor) 同步借用交付。旧 buffer／owned 接口最终交付时复制。缓冲不足的消息重试保留共享切片；重复消息索引可在同一额度内缓存。GetCacheStatistics 报告命中和 chunk 加载，包含未压缩加载。

随机读取校验整个加载的 chunk，包括尾部，可能比前缀读取更早报告损坏；仍不等于全文件验证。完整扫描保留声明、边界、CRC、结束标记和恢复语义，严格预校验在 chunk 验证后交付。顺序回放使用 cursor。
