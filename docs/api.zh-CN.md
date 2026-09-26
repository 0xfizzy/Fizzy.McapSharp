# API、生命周期与数据所有权

[English](api.md) | 简体中文

公共类型位于 `Fizzy.McapSharp` 命名空间。当前仅支持 .NET 8、Windows x64 进程；以文件路径为入口，不提供 Stream、异步或取消接口。消息编码、Schema 内容及时间基准由应用定义，库不解码业务数据。

## 写入文件

`McapWriter(path, options)` 创建新文件，拒绝覆盖已有路径。先注册消息所用的 Schema 和 Channel，再写消息；录制过程中可以注册其他 Channel。ID 由底层分配，等价注册可能返回已有 ID，不应假定 ID 连续或跨文件可复用。

| 方法 | 契约 |
| --- | --- |
| `RegisterSchema(name, encoding, data)` | 返回 `ushort` Schema ID，data 为原始字节。 |
| `RegisterChannel(topic, messageEncoding, schemaId = 0, metadata = null)` | 返回 `ushort` Channel ID；schemaId 为 0 表示无 Schema。 |
| `WriteMessage(channelId, logTime, publishTime, sequence, data)` | Channel 必须已注册；时间为 `ulong` 纳秒，序号为 `uint`，均由调用方提供。 |
| `WriteMetadata(name, metadata)` | 写入名称和字符串键值对。 |
| `WriteAttachment(name, mediaType, logTime, createTime, data)` | 写入附件及时间、媒体类型。 |
| `Flush()` | 调用底层 flush，不完成文件尾，也不等同于 Complete 的磁盘同步。 |
| `Complete()` | 完成 MCAP 尾部并同步文件；成功后重复调用无操作，其他写操作被拒绝。 |
| `Dispose()` | 释放资源，不隐式完成录制；未成功 Complete 的文件按未完成文件处理。 |

所有 Writer 操作由内部锁串行化；多线程的业务顺序仍由应用协调。输入 span 会复制，并在调用返回前消费，返回后可复用源缓冲区。原生调用及响应处理阶段的错误会使 Writer 终止，后续操作抛出 `InvalidOperationException`；应释放实例并选择新路径开始新录制，不在同一实例重试写入。

### 写入选项

| `McapWriterOptions` 属性 | 默认值 | 含义 |
| --- | --- | --- |
| `Compression` | `McapCompression.None` | 可选 None、Lz4、Zstd；压缩作用于 Chunk。 |
| `ChunkSize` | `4 * 1024 * 1024` | Chunk 目标字节数，必须大于 0，不是文件或消息大小上限。 |
| `UseChunks` | `true` | 是否将消息写入 Chunk。 |
| `EmitIndexes` | `true` | 控制摘要记录、消息索引和 Chunk 索引；关闭后仍可顺序读取。 |
| `Profile` | 空字符串 | 写入 MCAP profile，不替应用检查业务约束。 |

写入端开启底层支持的 Chunk、Data、Summary 和 Attachment CRC 计算；实际区段取决于选项与内容。

```csharp
using Fizzy.McapSharp;

var path = Path.Combine(Path.GetTempPath(), $"recording-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path, new() { Compression = McapCompression.Zstd }))
{
    var schema = writer.RegisterSchema("sample", "jsonschema", "{}"u8);
    var channel = writer.RegisterChannel("/sample", "json", schema);
    writer.WriteMessage(channel, 1_000, 900, 0, "{\"value\":42}"u8);
    writer.WriteMetadata("session", new Dictionary<string, string> { ["clock"] = "application" });
    writer.WriteAttachment("note.txt", "text/plain", 1_000, 900, "example"u8);
    writer.Complete();
}
```

## 读取与查询

`McapReader(path)` 检查平台和 ABI 并保存绝对路径，不在构造时读取文件。每次枚举独立打开原生 Reader，文件错误通常在开始或推进枚举时出现。Reader 本身不实现 `IDisposable`；枚举器需要释放，`foreach`（包括 break）会自动处理。

```csharp
var reader = new McapReader(path);
foreach (var message in reader.ReadMessages(new()
{
    Topic = "/sample", StartTime = 1_000, EndTime = 2_000
}))
    Console.WriteLine($"{message.Channel.Topic}: {message.Data.Length} bytes");
```

- `Topic = null` 表示所有 Topic，否则按完整字符串匹配，不支持通配符。
- 时间条件作用于 `LogTime`，区间为 `[StartTime, EndTime)`；null 表示不限对应边界。起点大于终点抛出 `ArgumentException`，相等表示空区间。
- 返回文件/Chunk 顺序，不按时间戳排序。有可用索引时按时间选取重叠 Chunk；无可用索引或存在 Chunk 外消息时顺序过滤。
- `ReadSchemas()` 和 `ReadChannels()` 包含没有消息引用的声明，按 ID 去重；Channel 的 Schema 可以为 null。`ReadChannels()` 先枚举 Schema，再枚举 Channel。
- `ReadMetadata()` 和 `ReadAttachments()` 分别读取元数据与附件，不受 `McapQuery` 过滤。

消息、Schema、附件的 `Data` 是托管 `byte[]`，枚举器释放后仍可使用；数组可变，不是不可变数据。读取会分配托管内存，大记录可能引起大分配或超过托管数组限制。

枚举期间 Windows 文件共享模式拒绝普通写入和删除。调用方仍须保持整个读取过程中文件稳定，尤其是多次枚举之间；打开前已存在的可写映射不在此保护之内。

## 完整性校验与恢复

`Validate()` 扫描整个文件，检查记录解析、存在的 Chunk/Attachment/Data/Summary CRC、记录边界及结束 magic。返回 `ulong` 校验扫描记录计数，**不是消息条数**。失败抛出 `McapException`。MCAP 的 CRC 为 0 表示未提供校验和，不能保证对应内容完整性；校验也不检查业务 Schema 语义。

普通读取拒绝未完成文件，但查询只检查经过的 Chunk 和解析到的记录，查询成功不证明整份文件完整。需要完整性检查时显式调用 `Validate()`。

`RecoverMessages(accept)` 恢复有效消息前缀，不修改源文件，不跳过损坏记录或 Chunk：

```csharp
var recovered = new List<McapMessage>();
var result = new McapReader(path).RecoverMessages(recovered.Add);
if (!result.IsComplete)
    Console.WriteLine($"Recovered {result.RecoveredMessageCount} messages: {result.Error}");
```

恢复遇到读取错误即停止；枚举结束后还会完整校验。必须检查 `IsComplete`，收到消息不代表文件完整。`RecoveredMessageCount` 为成功回调的消息数，完整成功时 `Error` 为 null。回调异常直接传播，不转换为恢复结果，已执行的回调不会回滚。尚未写入文件的缓冲内容无法恢复。

## 错误类型

| 场景 | 表现 |
| --- | --- |
| 非 Windows 或非 x64 进程 | `PlatformNotSupportedException` |
| 原生 DLL 缺失、不可加载或入口缺失 | .NET 原生加载异常，保留原始类型 |
| ABI 不匹配、原生文件或 MCAP 操作失败 | `McapException`，派生自 `IOException` |
| 参数检查失败 | 标准参数异常 |
| Writer 已完成、已失败或已释放 | `InvalidOperationException` 或 `ObjectDisposedException` |

内存分配、托管反序列化及回调等异常不保证包装为 `McapException`。原生边界见 [native.md](native.zh-CN.md)。
