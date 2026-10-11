# API、生命周期与数据所有权

[English](../api.md) | 简体中文

`Fizzy.McapSharp` 提供 MCAP 文件、Stream、缓冲区和 Sans-I/O 操作，支持 .NET 8、Windows x64 和 glibc Linux x64/ARM64，Linux 构建基线为 Ubuntu 22.04。不支持 macOS、musl 和 32 位进程。消息编码及纳秒时钟语义由应用定义；支持可取消的异步记录读取和时间排序；业务解码由应用实现。参见[官方 API 覆盖表](coverage.md)。

## 读写能力总览

先找到所需能力，再选择输入／输出、交付所有权及访问模式。下表列出本库能力；与官方 Rust API 的对应关系见[覆盖指南](coverage.md)。

| 能力 | 写入路径 | 读取路径 |
| --- | --- | --- |
| 消息 | header＋span、完整消息、prepared channel、批量及 lease 转写 | 自有结果枚举、调用方缓冲、借用回调、批次 lease |
| Schema 与 Channel | 显式注册，或完整消息自动声明 | 已遇声明查询、不可变描述、自有枚举 |
| Metadata | `WriteMetadata`、prepared operation | 枚举或索引 `ReadMetadata` |
| 附件 | 完整 payload、prepared operation、分段附件 | 枚举或索引 `ReadAttachment` |
| 原始记录 | `WritePrivateRecord` 写入私有操作码 | 顶层／展开记录、调用方缓冲、自有记录及记录视图 |
| 摘要与索引 | writer 选项控制生成；`Complete` 后访问摘要 | 摘要快照／游标、索引查询、chunk 游标及随机 seek |

可以读取任意记录，不代表提供通用的任意记录写入器。写入支持文件或 Stream；读取还支持复制／映射缓冲及直接 Sans-I/O。详见[写入](#写入录制)、[自有与原始记录读取](#自有记录摘要和原始记录)及[读取器适配](#官方读取器直接适配)。

## 选择写入路径

所有 payload 输入都在调用返回前同步消费。根据声明和载荷已有的表示形式选择路径；批量减少调用开销，但不提供原子性。

| 路径 | 声明与准备 | 适用场景 |
| --- | --- | --- |
| `WriteToKnownChannel(in header, span)` | 先注册 channel；预热路径托管分配为 0 B | 已知 channel 上重复写入，复用 payload 缓冲 |
| `WriteMessage(McapMessage)` | 读取当前描述并自动声明；便利处理允许分配 | 写入已有自有消息对象 |
| Prepared channel 的 `WriteMessage` | 一次准备不可变描述；写入时自动声明；预热写入托管分配为 0 B | 重复完整消息写入，避免反复序列化描述 |
| `WriteBatch(headers, storage, ranges)` | 先注册 channel；调用方提供连续 payload 存储与范围 | Payload 已位于同一存储区；不必仅为批量而重新拼接 |
| `WriteBatch(lease[, headers])` | 先注册目标 channel；可替换 header；跨 owner 共享保留载荷 | 不拼接 payload 地转发读取批次 |

准备操作允许分配。具体类型的 prepared 描述 同样可复用 schema/channel/metadata/attachment 控制描述。原生 writer、codec 和 Stream 成本仍存在。具体契约见[预准备写入](#完整消息与预准备写入)、[批量转写](#借用批次与-lease)。

## 选择消息所有权

先选择交付所有权，再优化分配。消息会话的查询与排序独立于交付方式；下列方法交付选中的消息，但结果形式和生命周期不同。

| 需求 | API | 托管分配与 payload 交付 | 生命周期 |
| --- | --- | --- | --- |
| 独立、可修改的结果 | `ReadMessages()` | 分配结果对象和最终 payload 数组；每条结果复制可变 schema 字节与 metadata | 独立于 reader |
| 复用调用方存储 | `ReadNext(buffer, ...)`、`ReadBatch(...)` | 库预热路径托管分配为 0 B；payload 复制到调用方缓冲 | 调用方拥有缓冲并决定覆盖时机 |
| 同步处理 | `ReadNext(visitor)`、`VisitMessages(...)` | 库预热路径托管分配为 0 B；无最终 payload 交付复制 | Span 在回调返回时失效；禁止重入 reader |
| 跨操作保留或转写 | `ReadBatchLease(...)` | 分配 lease 控制对象；共享 payload backing，不为批次重新拼接 | 访问期间保持 lease 存活；禁止并发释放 |

`McapMessageReader`（包括 snapshot 的 chunk 消息 reader）提供四种消息交付方式。`McapRawMessageReader` 提供缓冲区、回调、批次和 lease，但不提供隐式解析 channel 的自有消息枚举。`McapAsyncMessageReader` 提供 ReadBatchLeaseAsync，`McapAsyncRecordReader` 提供调用方缓冲区记录读取。Sans-I/O 使用各自协议的值类型事件。

`ReadMessages()` 的复制用于提供独立可变结果，并非消息读取必须创建自有数组。缓冲与借用路径返回 header，需要声明时单独查询；`GetChannelDescription` 提供可共享的不可变声明。选择交付方式不会消除解析器输入、解压、查询排序或索引成本。参见[分配保证](#校验恢复和分配承诺)。

长期持有 lease、逐条随机 seek 都是合法用法。[保留建议](#为保留结果选择存储)和[访问模式](#完整-chunk-随机访问)用于选择性能策略；生命周期和并发规则属于正确性要求。

## 选择访问模式

访问模式决定从哪里、按什么顺序读取，交付所有权决定怎样将结果交给调用方。两者分别选择，不是互斥的 API 类别；具体组合取决于 reader。

| 需求 | 路径 | 主要取舍 |
| --- | --- | --- |
| 按文件顺序处理录制 | 顺序消息／记录会话 | 增量输入，无需为全局排序收集结果 |
| 按 topic／时间过滤并按时间排序 | 使用可用索引的查询 | 可能缓冲重叠 chunk；索引不足时回退可能收集全部匹配结果 |
| 拒绝全局缓冲排序 | `QueryMessages` 配合 `AllowBufferedSort = false` | 仍允许文件顺序扫描及受支持的索引查询 |
| 必须使用索引 | `OpenIndexedMessages` | 不回退到顺序扫描或全局排序，而是失败 |
| 读取同一 chunk 内多条消息 | 一个 `OpenChunkMessages` 游标 | 在该游标内复用遍历 |
| 读取已知的一组离散消息 | `SeekMessages` | 单次调用内合并 chunk 加载；返回共享批次 |
| 跨调用反复访问 chunk | 复用配置缓存额度的 snapshot | 用保留存储换取更少的重复加载／解压 |

详见[查询选择](#选择查询路径)、[随机访问](#完整-chunk-随机访问)及[局部额度](#绑定层性能与局部限制)。以下章节分别定义各操作及其生命周期、失败和校验契约。

### 随机访问入口与输入存储

普通应用优先使用 `ReadMessages()` 返回的独立自有结果；有明确性能需求时，再选择调用者缓冲区、同步回调或 lease。既有所有权表说明四者的生命周期，交付形式不改变查询和排序语义。

| 需求 | 入口 | 输入所有权与成本 |
| --- | --- | --- |
| 按已知偏移读取原始记录或 chunk | 工厂的 `OpenRecordAccessor()` 或 Stream 重载 | 保留可定位源，随机操作保持源位置，不复制完整输入 |
| 独立于调用者字节的索引读取 | `McapIndexedReader.CreateCopied(bytes, options)` | 复制完整 MCAP 输入；返回的 reader 需要释放 |
| 独立于文件或 Stream 的索引读取 | 工厂或 accessor 的 `CreateCopiedIndexedReader()` | 复制完整 MCAP 区域；Stream 初始位置为起点，复制保持 accessor 的源位置 |
| 不复制完整文件的索引读取 | `McapIndexedReader.OpenMapped(path, options)` | 保留映射，所有相关游标和 lease 释放前文件必须保持不变 |

`McapRecordAccessor` 负责原始记录及索引访问；`McapIndexedReader` 持有输入并提供消息定位、chunk 游标和索引附件/元数据读取，不只是索引集合。两者均需释放。工厂 Stream 重载默认拥有源，除非指定 `leaveOpen: true`；复制完成的 reader 独立于源。`SeekMessage` 不移动其他游标，prepared 和缓存配置不改变输入所有权。

## 写入录制

文件创建语义：`McapWriter(path, options)` 原子地创建新文件，路径已存在时失败，保留已有文件内容。这是路径重载默认的防误覆盖行为，并非 MCAP 格式的要求。

`McapWriter(stream, options, leaveOpen: false)` 接受可写的可寻址或非寻址流，由调用方在打开流时选择创建或截断方式。若需显式覆盖文件，使用 `FileMode.Create` 和 `FileAccess.Write` 打开 `FileStream`，再传给 Writer；打开该流时就会立即截断已有文件。可寻址输出必须位于流末尾，当前位置作为 MCAP 偏移零点；非寻址写入使用原生 Chunk 缓冲。

```csharp
using Fizzy.McapSharp;

using var writer = new McapWriter(path, new() { Compression = McapCompression.Zstd });
var schema = writer.RegisterSchema("sample", "jsonschema", "{}"u8);
var channel = writer.RegisterChannel("/sample", "json", schema);
var header = new McapMessageHeader(channel, Sequence: 0, LogTime: 1000, PublishTime: 900);
writer.WriteToKnownChannel(in header, "{\"value\":42}"u8);
writer.Complete();
```

| 操作 | 契约 |
| --- | --- |
| `RegisterSchema(name, encoding, data)` | 返回原生分配的 ushort ID；首参数为 id 的重载指定非零 ID，等价内容可能去重。 |
| `RegisterChannel(topic, messageEncoding, schemaId, metadata)` | 返回 Channel ID；首参数为 id 的重载指定 ID，允许零。Schema ID 零表示无 Schema；同一 ID 的冲突内容会失败。 |
| `WriteToKnownChannel(in header, data)` | 使用消息头和 payload span 写入已注册 Channel，同步消费输入，不创建托管 payload 副本。 |
| `WriteMetadata(name, metadata)` | 写入字符串键值对。 |
| `WriteAttachment(name, mediaType, logTime, createTime, data)` | 一次写入附件。 |
| `StartAttachment(..., length)`、`WriteAttachmentBytes(data)`、`FinishAttachment()` | 按准确声明长度分段写附件；结束前拒绝其他写入操作，长度不符使 Writer 终止失败。 |
| `WritePrivateRecord(opcode, data, includeInChunks)` | 接受 0x80–0xFF 的私有操作码，可选择写入 Chunk。 |
| `Flush()` | 调用上游 flush，不完成 MCAP 尾部，也不保证持久化。 |
| `Complete()` | 完成 MCAP 格式并排空输出缓冲，不请求文件持久化。成功后重复调用无操作；输出保持所有权直至释放或 Stream 转移。 |
| `FlushToDisk()` | 成功 Complete 后，请求路径输出或直接传入的 FileStream 持久化；其他 Stream 抛出 NotSupportedException。 |
| `GetSummary()` | 成功 Complete 后复制上游完成摘要，允许分配。 |
| `Dispose()` | 释放资源，不隐式 Complete。 |

Writer 操作串行化，业务顺序由应用协调。配置允许的修改前安全拒绝不会终止 Writer；其他原生操作或 Stream 回调失败使 Writer 终止，需释放并开始新录制；进入原生调用前的参数/状态检查失败本身不使 Writer 终止。输入缓冲可在调用返回后立即复用。

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

### 完整消息与预准备写入

`WriteMessage(McapMessage)` 调用官方 `Writer::write`，按传入 ID 自动声明。预准备重载接收 `McapPreparedChannel`、匹配的 Header 和 payload Span，避免重复的托管序列化。准备时复制 Schema 与元数据，但不注册声明；描述符保持不可变，便利重载每次读取当前输入。

`McapPreparedSchema`、`McapPreparedChannelRegistration`、`McapPreparedMetadata`、`McapPreparedAttachment` 和 `McapPreparedAttachmentStart` 保存可复用的控制描述。将其传给对应的 RegisterSchema、RegisterChannel、WriteMetadata、WriteAttachment 或 StartAttachment 重载；注册返回非 nullable ID，其他方法返回 void。只有完整附件描述接受独立 payload，schema 字节在构造时捕获。用于自动声明消息写入的 `McapPreparedChannel` 与显式通道注册描述分开。预热后的 prepared 写入、分段附件、私有记录和 Flush 保留分配门禁。自有 McapMetadata/McapAttachment 可直接写入。Complete 完成格式，GetSummary 复制结果，OpenSummaryRecords 返回独立保留摘要的 reader；IntoInner 转移 Stream，不隐式完成格式。


### Writer 安全拒绝

`McapWriterOptions.SafeRejections` 是在构造时固定的标志组合。默认启用五种经核验的修改前拒绝：显式 Schema 注册使用 ID 零（`InvalidSchemaIdOnRegistration`）、显式 Schema 冲突（`ConflictingSchemaOnRegistration`）、任一 Channel 注册引用未知 Schema（`UnknownSchemaOnChannelRegistration`）、显式 Channel 冲突（`ConflictingChannelOnRegistration`），以及 header/payload 写入引用未知 Channel（`UnknownChannelOnMessageWrite`）。Prepared Schema/Channel 注册采用相同策略。可以选择任意子集；未知位在创建文件或取得 Stream 所有权前被拒绝。

被拒绝的调用仍抛出 `McapException`。仅当该拒绝发生后 writer 仍可使用时，`CanContinueWriting` 为 true；调用者修正输入后再重试。该值不保证后续操作或并发调用后的状态。自动声明的完整消息写入、附件长度错误、ID 耗尽、I/O、回调、压缩失败和 panic 仍使 writer 终止。库不自动重试。托管参数和状态预检查保持原有行为。

如需所有原生 writer 失败均终止，配置：

```csharp
var options = new McapWriterOptions { SafeRejections = McapWriterSafeRejections.None };
```

成功消息写入保持零托管分配契约；错误处理不属于该契约。



## 使用可复用缓冲读取消息

文件 `McapReaderFactory` 是工厂，构造时不打开文件；每次 `OpenMessages` 或 `OpenRecords` 创建独立的 McapMessageReader 或 McapChunkFlattener 和原生句柄。会话必须释放，建议使用 using。

```csharp
using var session = new McapReaderFactory(path).OpenMessages(new() { Topic = "/sample" });
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

`BufferTooSmall` 返回所需 payload 长度和消息头，不修改目标缓冲，也不消费待处理记录。空消息成功返回 `Success`，长度为零。`McapReadStatus.Success` 表示操作产生了结果或事件；具体内容由 opcode 或事件种类确定。EOF 可重复读取。错误会终止消息/记录推进；调用方缓冲接口不暴露原生地址；专用借用入口遵循下文生命周期契约。

过滤条件接受精确 Topic 或 Topics 集合，按 [StartTime, EndTime) 选择 LogTime。null 表示无界，起点大于终点抛异常，相等时不选中消息。顺序过滤保持文件顺序，时间排序通过下文的显式查询入口选择。

可定位时间排序查询在摘要声明和 Chunk 覆盖充分、无 Chunk 外消息时使用官方 `IndexedReader`。非默认线性解析选项使高层查询转为扫描；索引专用选项不暴露这些开关。缺少摘要／声明／统计、Chunk 覆盖不足、存在 Chunk 外消息或源不可定位，都可能使查询无法选择索引路径。仅缺少逐消息索引不一定阻止索引读取：仍可解析选中的 Chunk 来取得消息。索引损坏及解析／I/O 错误可能直接失败，而非回退。直接 Sans-I/O 索引读取不使用扫描排序回退。

### 选择查询路径

先选择执行能力；同一能力中，省略选项与传入空选项等价。

| 入口 | 条件与默认值 | 执行方式 |
| --- | --- | --- |
| OpenMessages / ReadMessages | McapMessageFilter；文件顺序 | 顺序扫描；增加 topic/time 过滤不改变执行方式 |
| OpenIndexedMessages | McapIndexedQuery；LogTime 升序 | 必须有可用索引，不回退扫描或全量排序 |
| QueryMessages / ReadQueryMessages | McapQuery；LogTime 升序 | 可用时使用索引，否则按显式回退策略处理 |

StartTime 包含边界，EndTime 不包含边界。Topic 与 Topics 互斥；空 Topics 不选择消息。升序和逆序均不规定同时间戳的次序。索引 File 顺序即使没有过滤条件，也仍要求可用索引。

McapQuery.AllowBufferedSort 默认为 true；无可用索引的时间排序会先将全部匹配消息收集到原生内存，没有自动磁盘溢写。设为 false 时，在收集前以 NotSupportedException 拒绝回退。索引专用入口不可用时抛 McapException。McapIndexedQuery 不含回退控制，其 RecordLengthLimit 与线性解析选项分开。缓存和排序内存规则见后文；索引查询成功不等于全文件校验。

GetChannel、GetSchema 和不可变描述查询均不推进读取。顺序 reader 暴露已经遇到的声明；索引和 chunk 消息 reader 也可以使用摘要声明。未知 ID 不终止读取。推进失败后，继续读取及查询声明统一抛 InvalidOperationException；释放后抛 ObjectDisposedException。已经返回的副本、不可变描述和 lease 仍有效。

## 自有记录、摘要和原始记录

文件工厂的 ReadMessages、ReadSchemas、ReadChannels、ReadMetadata、ReadAttachments 各自打开独立扫描；Stream 分类枚举重载在枚举期间拥有增量读取会话。分类扫描验证所有展开记录，但只为目标记录创建托管结果；跳过的 payload 不产生托管结果数组。reader 枚举消费当前游标。消息、schema 和附件的字节数组可变且独立；每条自有消息防御性复制可变声明。

GetSummary 返回完整的统计、索引及 Schemas/Channels 字典，没有摘要时返回 null。Schemas 只列出摘要中重复的 schema；channel 可以引用在数据段找到的 schema，而不把它加入重复 schema 映射。同一个摘要内，channel 可以共享 schema 对象；不同摘要调用的结果相互独立，且在生产者释放后仍有效。writer 的完成摘要描述内存中的写入结果，即使输出禁用了摘要记录。模型沿用普通 .NET record 相等语义，数组和字典不隐含 Rust 式深内容相等；直接构造模型保留传入引用。

OpenLinear 保留编码后的 Chunk，OpenRecords 返回 McapChunkFlattener；两者均提供 ReadNextRecord 和自有 ReadRecords，记录体不含 opcode 和八字节长度前缀，未知/private 数据原样保留。McapChunkReader 接受不含外层记录头的单个 Chunk 编码体，只验证局部输入。消息 reader 的 ReadNextMessageRecord 返回 22 字节消息头及 payload，与其他消息交付方式共享待处理状态，包含空 payload 和缓冲不足重试。

## Stream 会话与所有权

Stream 输入使用 McapReaderFactory.OpenMessages(stream, filter, leaveOpen)、OpenRecords(stream, leaveOpen: leaveOpen) 或 OpenLinear。初始 Stream 位置是 MCAP 偏移零；读取增量进行，不预先复制整个输入。从该位置到结尾应包含一份 MCAP。

一个 Stream 同时只能由一个 MCAP 会话占用。会话活动期间，调用方不得自行定位、截断、读写或释放 Stream。回调在发起线程同步执行，拒绝回调重入同一 Writer/会话；回调异常在退出原生边界后传播。leaveOpen 默认 false，释放会话时关闭流，true 时保留。

显式释放报告所拥有 Stream 和原生资源的释放失败。清理只尝试一次，后续 Dispose 不重试。只有释放或所有权转移成功才解除 Stream 独占；所拥有 Stream 释放失败后，不得被另一个 MCAP 会话接纳。IntoInner 只在原生状态成功释放后返回 Stream，不隐式完成格式，也不释放转移的 Stream。终结器是不抛异常的后备机制，不决定显式释放的错误策略。

OpenRecordAccessor 返回 McapRecordAccessor，保留可寻址源的位置，支持原始记录／Chunk／索引、摘要及独立快照创建，不预先复制整份输入。该 reader 的解析或 I/O 失败为终止状态，与顺序 reader 各自拥有失败边界。操作前参数检查、缺失声明查询和缓冲协商不终止读取。操作与位置恢复同时失败时报告两项错误，安全释放后通过新 reader 重复工作。

OpenRecordAccessor 要求可寻址输入，在取得所有权前拒绝非寻址 Stream。McapReaderFactory.GetSummary(stream) 可寻址时使用随机访问，否则扫描到 EOF。更换能力需要新 reader，不提供重置。复制／映射／路径输入没有可由 IntoInner 返回的托管 Stream；Stream-backed reader 保留显式转移能力。

映射文件在读取、校验和恢复期间必须保持不变。Windows 在映射期间拒绝普通写入/删除打开，但无法排除之前已有的可写映射；Linux 不强制互斥。并发截断映射文件可能终止进程。

## 校验、恢复和分配承诺

`McapReaderFactory.Validate()` 扫描全文件，检查记录解析、存在的 Chunk/Attachment/Data/Summary CRC、记录边界和结束 magic，返回扫描记录数而非消息数。展开校验拒绝非法／冲突的声明 ID、引用未声明非零 Schema 的 Channel，以及引用未声明 Channel 的消息。CRC 为零表示未提供校验和；不检查业务 Schema 语义。

McapReaderOptions 控制 magic、尾部字节、Chunk 输出、CRC 和记录长度限制。可选 CRC 和尾部检查默认关闭。高层展开记录和解析声明的消息读取始终验证记录体、声明 ID／冲突及引用，独立于 Strict 或任意单个 CRC 开关，适用于 Stream、复制、映射和异步输入。顶层读取不验证隐藏 Chunk 声明，局部 Chunk 与原始消息有明确范围；原始消息收集声明，但允许消息引用未知 channel。直接 Sans-I/O 线性事件仍为原始记录。Strict 只增加具名完整性检查。Chunk flattener 拒绝 EmitChunks。

IsScanComplete 仅表示 EOF；IsFullyValidated 和 ValidateRemaining 只由严格完整展开扫描建立，索引、排序回退和局部 Chunk 不足以证明全文件验证。摘要查询声明不计作顺序扫描已经遇到的声明。验证保留独立 schema 内容和 channel 描述，空间随声明字节和 metadata 增长；16 位 ID 上限不代表恒定内存。snapshot 缓存只由 McapIndexedReaderOptions 控制。

### 创建时验证能力

消息 reader 和 chunk flattener 的 `ValidationCapability` 在创建时固定，失败和释放后仍可查询。它描述能力范围，不表示验证成功。

| 能力 | 含义 | ValidateRemaining / 消息恢复 |
| --- | --- | --- |
| `PartialInput` | 索引、缓存排序或 chunk 局部结果 | 在继续消费前拒绝 |
| `FullScan` | 未配置完整严格检查的 expanded 顺序扫描 | 在继续消费前拒绝 |
| `StrictFullScan` | 从创建起执行严格检查的完整 expanded 顺序扫描 | 会话可用时支持 |

顺序 topic/time 过滤仍验证被跳过的记录。能力拒绝不执行额外 I/O，不丢失待重试记录，也不使正常 reader 失败；已失败或已释放的 reader 仍拒绝操作。`ValidateRemaining` 完成从创建起已启用的检查，不补验此前跳过的检查。`RecoverMessages` 从当前游标继续，只统计本次交付的匹配消息。查询可能在返回前已执行扫描排序，但返回的结果能力仍为 PartialInput。需要独立完整操作时，优先使用工厂的 Validate 和 RecoverMessages。

RecordLengthLimit 在线性路径限制记录体；indexed 构造还检查 chunk 压缩及解压大小，后续读取检查相关记录长度。修改 Sans-I/O indexed 限值不重复构造阶段检查。摘要路径限制摘要记录体，并可能根据已知文件大小进一步约束读取。这些限制都不是总内存上限。


严格消息 reader 的 RecoverMessages(accept) 交付有效前缀，并将原始结构化解析错误保留在结果中；工厂重载自动打开严格文件顺序 reader。前缀抢救应使用顺序读取；显式排序查询可能在构造阶段即失败。检查 IsFullyValidated 与 Error，已有消息不证明完整性。Stream、原生 I/O、Binding 和回调异常直接传播，不作为抢救结果；解析失败和 accept 回调异常均终止 reader，回调异常保留原对象。已返回结果仍有效。这不允许重试任意 I/O，也无法恢复从未写入源的字节。

初始化、注册和预热后，正常 WriteToKnownChannel 与缓冲区 ReadNext 必须严格产生 0 B 托管分配。这是热路径的硬性契约，覆盖跨 Chunk/压缩边界、中途声明、缓冲不足重试和 EOF。读取仍将原生数据复制到调用方内存，并非零拷贝；也不保证进程没有 GC 或 Rust 没有堆分配。调用方扩容、自有记录枚举、描述查询、启动和错误路径不在承诺内。用户 Stream 自身可能分配，本库只承诺桥接层的分配行为。

不支持的平台抛 PlatformNotSupportedException；原生加载错误保留 .NET 类型；原生操作/ABI 失败抛 McapException（继承 IOException）。托管参数/状态检查使用标准异常，已释放对象抛 ObjectDisposedException。参见 [ABI](native.md) 与[分配验收和构建](development.md)。

文件总长度不作为记录或解压后 Chunk 的大小上限。高压缩率 Chunk 解压后的大小可以超过文件大小，但仍需要足够可用内存并受上游解析器限制。


## 官方读取器直接适配

McapReaderFactory 本身不拥有资源，按文件或 Stream 构造具体 reader。复制输入与映射输入由对应 reader 的构造函数和 OpenMapped 提供。输入所有权不改变结果能力；内部核心及模式编号不属于公开 API。

| 托管 reader | 官方对应项／范围 |
| --- | --- |
| McapLinearReader | read::LinearReader；顶层编码 Chunk |
| McapChunkReader | read::ChunkReader；单个 Chunk 编码体 |
| McapChunkFlattener | read::ChunkFlattener；展开记录，应用绑定库统一高层验证基线 |
| McapRawMessageReader | read::RawMessageStream；允许消息引用尚未声明的 channel |
| McapMessageReader | read::MessageStream；解析声明的消息及缓冲区／回调／批次／lease |
| McapSummaryRecordReader | 基于保留 Summary 的绑定扩展 |

上游 read 模块有五种 reader 加 Summary，第六种托管 reader 明确属于摘要记录扩展。复制构造只复制一次完整输入，OpenMapped 保留映射。需要无 magic 输入时显式设置 SkipStartMagic/SkipEndMagic。仅记录类型没有消息方法；原始消息类型没有隐式解析声明的自有枚举。推进失败均为终止状态，之前已返回的结果保持有效。

`McapRecords.Parse` 返回 `IMcapRecord`：标准记录为对应强类型自有模型，未知记录为 `McapRawRecord`。通过具体结果类型匹配访问字段；仅实现该接口不代表记录属于已知类型或已经解释字段。`McapRawRecord` 明确表示操作码及未解释的自有记录体。`McapRecordView.Parse` 使用官方 `parse_record` 校验，在调用方托管内存上提供视图。校验只针对 Parse 时的字节；使用视图及字段游标期间必须保持底层内容不变，也不得通过其他可写别名修改。标量属性和 `Fields` 游标可零分配读取 UTF-8、映射、数组和二进制字段。`ToOwned` 显式复制；`ReadFooter` 和 `GetCompressedDataOffset` 直接调用上游辅助函数。

`CreateCopiedIndexedReader()` 复制可定位源并读取官方摘要，不移动随机访问源的位置；也可用 `McapIndexedReader.CreateCopied(bytes)`。快照独立于原会话，原生内存需求与文件大小成比例。提供消息定位、指定 Chunk 消息枚举、消息索引、按索引读取 Metadata/Attachment、Footer、描述和摘要。随机操作使用调用者提供的索引字段，包括长度及通道偏移映射，不按 offset 替换为摘要中的索引。Metadata/Attachment 可在没有摘要时使用调用者提供的索引读取；Chunk 消息和消息索引读取仍需摘要声明。对应上游函数未使用的索引字段不会增加额外校验。调用期间不得修改索引的映射。`OpenChunkMessages(index)` 返回独立、惰性、可释放的 McapMessageReader，快照释放后仍然有效。游标共享不可变原生输入和摘要，不再次复制文件。`ReadChunkMessages` 的每个枚举拥有独立游标。随机操作不移动这些游标。缓冲区重载不分配托管对象；Metadata/Attachment 输出记录体，消息索引每项 18 字节小端编码：u16 通道 ID、u64 LogTime、u64 Chunk 内偏移。自有消息索引结果为每个已验证通道索引保留分组，包括空组；行缓冲只表达条目，需要区分空组身份时使用自有结果。不足时不修改目标。`OpenSummaryRecords()` 提供摘要字段及声明的缓冲区游标。

### 预编译 Chunk 索引

重复随机访问时，构造一次 `McapPreparedChunkIndex(index)`，使用结束后释放。构造复制索引字段和通道偏移映射，仅编码一次并由官方 Rust 实现解析一次。构造期间不要修改映射；后续修改不会影响 prepared 索引。`SeekMessage`、`OpenChunkMessages`、`ReadChunkMessages`、`ReadMessageIndexes` 和 `GetCompressedDataOffset` 的 prepared 重载消除重复索引编码、临时原生 scratch 和索引解析。原有重载仍观察调用方当前字段。编码器对具体 `Dictionary` 使用无装箱枚举，对具体 `SortedList` 按键／值索引访问；两条路径都只访问现有条目。其他实现（包括 `ReadOnlyDictionary` 和 `SortedDictionary`）使用通用查找回退路径。对通用 `IReadOnlyDictionary` 实现中的稀疏高 ID 映射进行重复调用时，优先使用 prepared 索引：非 prepared 编码器可能遍历 ushort 键域，每次最多查找 65,536 次，以避免枚举器分配。准备操作只捕获一次映射，避免重复执行这部分工作。回退路径避免创建接口枚举器，但调用方映射或比较器本身仍可能在查找期间分配；这些分配不受库控制。

每个 prepared 描述符保留一个托管通道 ID 数组，用于重建 owned 索引结果中的空组；每个通道占 2 字节，不含数组头。原生存储还保留完整已解析索引和编码缓存键。Dispose 释放原生所有权；托管 ID 数组仍随描述符保留，直到垃圾回收。

prepared 索引可跨 snapshot 复用。每次操作仍执行原有文件范围及摘要要求；构造不验证某一具体文件。每个涉及的描述符的调用（包括 `SeekMessages` 批量调用）与释放串行执行。snapshot 借用消息回调中禁止对任何 snapshot 实例执行操作（包括 `Dispose`），也禁止使用或释放任何 prepared 索引，避免逆转先描述符、后 snapshot 的锁顺序。子游标在 prepared 索引和 snapshot 释放后仍可用。消息索引上游 helper、缓存未命中和解压仍可能分配，不能据此保证总原生分配为零。

`McapSansIoLinearReader`、`McapSansIoSummaryReader`、`McapSansIoIndexedReader` 分别公开独立协议。Linear 仅有 Read/Record/End；Summary 仅有 Read/Seek/End；Indexed 仅有 ReadChunk/Message/End。Summary 使用无目标缓冲区的 `NextEvent(out event)`，仅 Summary 公开 `NotifySeeked`；Indexed 只通过 `InsertChunkData` 提供请求范围的完整压缩数据，不含 Chunk 外层头，并保留 `SetRecordLengthLimit`。Linear 和 Summary 的 `SupplyInput(data)` 只回应 Read。定位由 `McapSeekRequest` 表达 Begin 的无符号 AbsolutePosition 或 Current/End 的有符号 RelativeOffset，不适用的属性抛 InvalidOperationException。已完成的摘要解析器提供 GetSummary、OpenSummaryRecords 和 CreateIndexed。记录和消息复制到调用者缓冲区；不暴露原生指针。内置 Lz4/Zstd 仍由上游实现，不增加自定义解压器注册。

## 异步记录和错误

McapAsyncRecordReader.ReadNextRecordAsync 返回 ValueTask<McapRecordReadResult>；McapAsyncMessageReader.ReadBatchLeaseAsync 返回保留消息，不进行查询排序。能力在构造时确定，均以 .NET Stream 异步 I/O 驱动官方 Sans-I/O，不引入 Tokio。展开记录体和声明引用在交付或缓冲协商前验证，不依赖 CRC 开关。消费在途操作后可以查询已经遇到的不可变 schema/channel 描述；顶层读取不能看到 Chunk 内声明。异步 EOF 不报告全文件验证成功。

每次只允许一个在途操作，每个 ValueTask 必须且只能消费一次，之后才能读取、查询、转移或释放。取消及 I/O/解析错误终止 reader。leaveOpen、Stream 独占和转移沿用同步契约。DisposeAsync 执行同步释放，调用所拥有 Stream.Dispose 并返回已完成 ValueTask；不等待在途工作，不调用 Stream.DisposeAsync。

复用完成源和 continuation，在预热后的实际 I/O 挂起路径也提供 0 B 托管分配。门禁同时计量调用线程和专用 I/O 线程，包含直接 await 循环。完成通知可在 I/O 线程内联恢复调用方，库不强制派发 ThreadPool。第三方 Stream、调用方 await 机制、初始化、错误、扩容及自有结果不在保证内；原生分配不受此保证约束。

`McapException.Kind` 对应全部上游错误变体，`Details` 保留结构化字段；封装层错误使用 `Binding`，原始 Stream 异常仍保留。 原生错误文本有界：消息最多保留 256 个 UTF-8 字节，详情文本值最多保留 128 字节，均在字符边界截断。缩短的文本带 `[truncated]`，对象详情同时设置 `truncated: true`；数值字段保持精确。原生 OS 错误通过 `osCode` 和固定格式消息报告，不分配本地化 OS 文本。 Parse 错误保留根因文本，不序列化 parser 的装饰性回溯。

## 借用、批次与 lease

Owned 结果保持独立：`McapMessage.Data` 仍为 `byte[]` 副本，可变声明仍防御性复制。`ReadNext(McapMessageVisitor)`、`VisitMessages(visitor, maxMessages)` 接收 `in McapMessageHeader` 与 `ReadOnlySpan<byte>`，消除最终交付复制。返回 false 会在当前消息后正常停止。Span 仅在回调期间有效；禁止回调内读取、seek、查询状态或释放同一 reader，可以写入另一个 writer。异常在正常 ABI 返回后重抛。snapshot 回调还禁止操作任何 snapshot 实例或 prepared Chunk 索引，见 [预编译 Chunk 索引](#预编译-chunk-索引)。`GetChannelDescription` 返回可共享的不可变声明快照。

`WriteBatch(headers, payloadStorage, ranges)` 一次加锁、一次 ABI 调用，同步消费连续共享载荷。开始前检查数量、范围及全部 Channel。成功返回数量，`McapBatchWriteException.CompletedCount` 不包含可能部分落盘的失败记录。批次不原子；原有安全拒绝配置继续适用，I/O、压缩及推进后的失败终止 writer。返回后可复用输入缓冲。

`WriteBatch(batchLease)` 写入 lease 原有的 header 和 payload；`WriteBatch(batchLease, headers)` 为每条消息替换完整 header，数量必须等于 `batchLease.Count`。先注册目标 schema/channel，需要重映射时提供替换后的 ChannelId；库不推断声明或映射。两个重载均一次 writer 加锁、一次 ABI 调用，直接引用不同存储 owner 的 payload 而不拼接，并在同步调用期间保持 lease 句柄有效。不转移所有权或修改 lease；不得与写入并发释放 lease。null、已释放 lease 和 header 数量不匹配在原生写入前拒绝，不使 writer 失败。Channel 预检查、安全拒绝及已完成前缀遵循上述批次契约。两个重载均有预热后的零托管分配门禁；创建输入 lease 的分配单独计算。

`ReadBatch(headers, ranges, payloadStorage)` 将完整消息写入调用方缓冲，返回数量、使用字节、停止原因和下一条所需容量。后续记录失败时，调用抛异常并终止会话；缓冲可能已写入前缀，但不返回前缀数量，因此必须丢弃本次调用的全部输出。已经执行的 visitor 回调副作用不回滚。失败的 lease 读取不发布部分批次；此前成功返回的 lease 仍有效。空间不足时保留下一条。借用、调用方批量读取和批量写入都有预热后的 Release 零托管分配门禁。

`ReadBatchLease` 返回 `McapMessageBatchLease`，默认最多 256 条、软目标 4 MiB。在加入整条消息后检查目标，因此批次可能超过目标，包括单条消息已超过目标的情况。通过 GetHeader、GetPayload、CopyTo、RetainMessage(index) 访问或保留消息，不产生逐消息载荷数组。批次可引用多个 chunk，不重新拼接。Lease 在 reader 推进、reader 释放和缓存淘汰后仍有效；Dispose 幂等，私有 SafeHandle 提供终结兜底。Retain 的消息 lease 单独释放。

Span 使用期间用 using 保持 lease 存活。访问入口拒绝已释放 owner，但已有 Span 无法撤销。禁止访问与 Dispose 并发；可在调用者同步下跨线程转移。所有相关 reader、子游标和 lease 释放前，映射文件必须保持不变。

ReadBatchLease 返回批次或 EOF 时的 null。McapAsyncMessageReader.ReadBatchLeaseAsync 等待 I/O 并支持取消，必须先消费 ValueTask 才能执行下一操作、查询或释放。取消终止 reader，但已有 lease 仍有效。异步记录与消息能力属于不同类型；两种展开 reader 均可查询已经遇到的不可变 schema/channel 描述，包括未引用 schema。未知 ID 不终止读取，失败或已释放 reader 拒绝查询，既有描述在释放后仍有效。消息 reader 拒绝 EmitChunks。不创建预取队列、溢写文件或自动文件分段。

异步 lease 读取复用完成源和 continuation。每个交付批次允许分配 lease 与 SafeHandle 对象；I/O 挂起无需再创建逐操作 async 状态机。已有消息的批次在 parser 下一次请求输入时返回，即使尚未达到消息数或 payload 目标，避免仅为填满批次而等待额外 I/O。Stream 实现自身仍可能分配。取消释放 parser 资源前必须先消费输入完成结果。

## 绑定层性能与局部限制

性能目标针对绑定层引入的分配与 payload 复制，不限制官方解析器、writer 或 codec 的内部分配。借用回调和 lease 引用稳定存储；缓冲区读取复制到调用方，便利 API 创建独立自有结果。复制构造拥有输入副本；`OpenMapped` 共享映射，Stream 支持增量输入。初始化、描述符、lease 控制对象和错误路径可以分配。

同步与异步输入按 parser 当前完整需求预留空间，I/O 传输大小单独控制，避免短读导致反复扩容。这不是整源预缓冲，也不是总内存上限。一个保留切片可能使整个 chunk 或映射继续存活；限制在途工作时，应同时考虑存储大小与批次数。缓存额度和批次 payload 目标不涵盖全部外部保留字节。

| 控制项 | 计量对象与默认值 | 边界行为与排除项 |
| --- | --- | --- |
| `McapQuery.MaxBufferedSortBytes` | 选中消息的逻辑 payload 长度总和，加描述符数组已分配容量的字节数；null 表示不设置收集额度 | 超额使回退构造／读取失败，不溢写或交付部分排序结果。即使共享 backing，每个选中范围仍计入自身长度。排除 backing 放大、紧凑化时新旧存储重叠、parser/codec 内存和 RSS；不适用于索引 reader 的 chunk 缓冲。 |
| `McapIndexedReaderOptions.MaxRandomAccessCacheBytes` | 局部 LRU 对保留 chunk 存储、描述符、键及索引字节的计费；0 关闭保留 | 超额条目加载后不进入缓存。淘汰只释放缓存引用，不释放调用方 lease。排除分配器开销、解析临时空间、snapshot 输入及外部保留 owner。未压缩映射 chunk 按逻辑解压大小计费。 |
| `ReadBatchLease` 数量和 payload 目标 | 默认最多 256 条；逻辑 payload 软目标 4 MiB | 加入整条消息后检查字节目标，允许越过目标。不限制 backing 容量或所有在途批次。异步批次可能在输入请求时提前返回。 |

snapshot 缓存由 McapIndexedReaderOptions 控制。McapIndexedQuery 不提供排序回退选项；McapQuery.AllowBufferedSort=false 完全拒绝回退收集。RecordLengthLimit 的检查范围取决于能力，索引路径还包含 chunk 大小，详见下方说明。这些局部控制都不是进程总内存上限。

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

Lease 保留 backing 所有权，而不只是可见的 payload 范围。`RetainMessage` 共享该所有权，不会裁剪分配。小消息可能保留整个解压 chunk、完整输入副本或文件映射。一个批次可能引用多个 owner，多个批次也可能共享同一个 owner。

Dispose 释放该所有者持有的引用；所有引用释放后，存储才可回收。其他 lease、parser、snapshot、子游标或缓存可能仍在引用它，因此复制一条切片不保证立即回收。Snapshot 本身持有输入，直到它及所有依赖 owner 都释放相关引用。映射大小是地址空间，不等于驻留内存测量值。

为控制流水线保留量，除批次数外，还应根据不同 backing 的大小与保留时长限制在途工作。能接受保留整个 backing 时，长期共享仍然合法；复制稀疏结果是性能选择，不是生命周期要求。

回退排序在发布结果前选择存储。按连续遇到的 owned backing 对选中消息分组，在 owner 切换或扫描结束时保留密集组、紧凑化稀疏组。当前内部策略在容量至少为选中 payload 的四倍、且该组 backing 容量可减少至少 256 KiB 时复制。紧凑段以消息边界划分，目标 256 KiB；超大消息独立成段，空 payload 不保留 backing。映射等外部存储继续共享。这些阈值属于实现选择，不是公共配置或性能保证。

紧凑化保持消息顺序、重试与 lease 寿命。选中 payload 只复制一次，期间新旧存储暂时并存；其他 owner 可能延迟实际回收。排序仍先收集结果再交付，其逻辑额度不覆盖瞬时复制、backing 保留放大、parser/codec 内存或进程 RSS。

### 完整 chunk 随机访问

缓存按完整调用方索引语义区分条目，映射插入顺序不影响身份，命中不重复解压。未压缩映射 payload 引用映射范围，压缩 payload 共享解压存储。缓冲不足重试保留共享切片。

`SeekMessages(ReadOnlySpan<McapMessageSeekRequest>)` 在本次调用内将同一 chunk 的请求分组，每个不同 chunk 加载一次，并按原请求顺序返回批次（包括重复项）；失败不交付半批。`SeekMessage(preparedIndex, entry, visitor)` 提供借用交付，其回调禁止使用或释放任何 snapshot 实例或 prepared Chunk 索引；缓冲区及自有结果 API 在最终交付时复制。`GetCacheStatistics` 报告命中和 chunk 加载次数。

按访问模式选择复用范围：

| 访问模式 | 方法 | 复用范围 |
| --- | --- | --- |
| 单条独立查询 | `SeekMessage` | 缓存未命中时加载完整目标 chunk；关闭缓存可避免缓存保留 |
| 同一 chunk 内多条消息 | `OpenChunkMessages` | 在同一游标内遍历一次；重新打开游标会开始新的遍历 |
| 已知的一组离散请求 | `SeekMessages` | 本次调用内合并加载，即使缓存关闭也有效；下一次调用属于另一批次 |
| 跨调用反复访问 | 复用配置了 `MaxRandomAccessCacheBytes` 的 snapshot | 保留近期 chunk，直到淘汰或 snapshot 释放 |

工作集是缓存复用期间会反复访问的那些 chunk。按解压存储及描述符成本配置额度，不能按压缩文件字节数估算。工作集超过额度可能导致淘汰与重复解压；这种访问仍然合法。通过 `GetCacheStatistics()` 的命中和 chunk 加载次数判断复用效果。Prepared index 减少重复描述符编码／解析，不负责避免缓存未命中时的解压；复制返回的 payload 也不会缓存源 chunk。

随机读取校验完整目标 chunk，可能比前缀读取更早报告尾部损坏，但不代表全文件校验。补丁边界与证据见[本地补丁](patches.md)。

## 结构化错误字段

`McapException.Kind` 和已公布的 Details 字段名/类型是稳定契约；以后可能增加字段或错误种类。调用者应忽略未知字段，并为未知种类保留默认处理。Message 以及 source/name/description 等文本用于诊断，可能被截断或改写，不能依赖精确措辞或据此判断重试。写入继续权限只遵循 CanContinueWriting 的既有契约。

| Kind | Details 对象字段 |
| --- | --- |
| `AttachmentTooLarge` | `excess`、`attachment_length`：无符号数 |
| `AttachmentIncomplete` | `current`、`expected`：无符号数 |
| `BadAttachmentCrc`、`BadChunkCrc`、`BadDataCrc`、`BadSummaryCrc` | `saved`、`calculated`：无符号数 |
| `BadAttachmentLength`、`BadChunkLength`、`BadSchemaLength` | `header`、`available`：无符号数 |
| `PrivateRecordOpcodeIsReserved` | `opcode`：无符号数 |
| `ConflictingChannels`、`ConflictingSchemas` | `name`：字符串 |
| `UnknownChannel` | `sequence`、`channel_id`：无符号数 |
| `UnknownSchema` | `name`：字符串；`schema_id`：无符号数 |
| `UnsupportedCompression` | `compression`：字符串 |
| `DecompressionError` | `description`：字符串 |
| `ChunkBufferTooLarge`、`ChunkTooLarge` | `length`：无符号数 |
| `RecordTooLarge` | `opcode`、`len`：无符号数 |
| `BadChunkStartOffset` | `offset`：无符号数 |
| `Parse`、`Io` | `source`：字符串；存在原生 OS 错误码时包含有符号 `osCode`，source 使用固定文本 |
| 原生 OS 错误的 `Binding` | `source`：固定字符串；`osCode`：有符号数 |
| 本地 allowance 的 `Binding` | `resource`：字符串；`limit`、`requested`：无符号数 |
| 操作与位置恢复同时失败的 `Binding` | `code` 为 `"operationRestore"`；`operation`、`cleanup` 为含 kind/message/details 的嵌套错误对象 |
| 其他上游种类 | 空对象，除非增加截断标志 |

没有结构化信息的原生错误使用 JSON null；仅以消息构造的托管异常其 Details 为 Undefined。对象可能带 `truncated: true`，没有该字段表示未报告截断。Message 上限 256 UTF-8 字节，文本详情上限 128 字节，含 `[truncated]` 后缀；数值保持精确。原始托管 Stream 异常可能直接传播，不包装成 McapException。普通 record 保留 .NET 数组/集合相等规则，IReadOnlyDictionary 不代表深度不可变；需要不可变共享时使用声明 description。
