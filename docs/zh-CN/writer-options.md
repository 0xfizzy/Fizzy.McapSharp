# 压缩、分块与索引配置

[English](../en/writer-options.md) | 简体中文

面向完成[写入指南](writing.md)、需要选择录制布局的应用。先使用默认值，再按具体存储或读取需求调整。配置在构造 writer 时固定。精确默认值及选项优先级见 [API 契约](api.md#写入选项)和 [McapWriterOptions 英文参考](xref:Fizzy.McapSharp.McapWriterOptions)。

## 从默认值开始

默认使用 Zstd 压缩、1 MiB 未压缩大小目标的 Chunk、上游默认启用的索引/摘要输出，以及绑定默认启用的 CRC 生成。省略选项与传入空选项等价。可空输出开关保留上游设置，除非由其规定的组选项设置；null 通常不等于 false。

```csharp
var options = new McapWriterOptions
{
    Compression = McapCompression.Lz4,
    ChunkSize = 1024 * 1024
};
using var writer = new McapWriter(path, options); // path must not exist
// Register declarations and write as in the writing guide.
writer.Complete();
```

这段配置演示显式选择，不代表推荐的性能最优值。调整前应使用有代表性的载荷和实际下游查询进行测量。

## 选择压缩与 Chunk 大小

`None`、`Lz4` 和 `Zstd` 选择 Chunk 压缩。`UseChunks = false` 时没有 Chunk 压缩和 Chunk/消息索引。压缩级别依赖具体 codec；Zstd worker 设置控制压缩工作线程，不控制 reader 并发。

较大 Chunk 可以减少 Chunk 数和索引条目，但可能增加活跃存储，以及稀疏读取时加载/解压一个 Chunk 的工作量。较小 Chunk 的取舍相反。`ChunkSize` 是目标，不是记录大小或内存硬上限，完整记录可以超过它。null 大小禁用按大小自动刷新 Chunk，并不建立有界缓冲。应结合吞吐量、录制时长和随机读取延迟选择。

## 为目标读取方式保留索引

| 需求 | 配置考虑 |
| --- | --- |
| 按文件顺序扫描 | 不需要消息/Chunk 索引 |
| 按索引进行时间查询与消息定位 | 保留 Chunk，以及所选索引入口要求的索引和声明 |
| 按索引读取元数据或附件 | 保留相应摘要索引 |
| 检查摘要 | 保留需要的统计和重复声明 |

`EmitSummaryRecords` 统一选择统计、摘要索引和重复声明。单独的非 null 开关覆盖该组设置。它不控制 `EmitMessageIndexes` 或 `EmitSummaryOffsets`。关闭某类输出并不是通用的“无索引”或“不保留索引”模式。

通过[查询选择](queries.md)决定是否接受顺序回退。例如，`UseChunks = false` 配合 `EmitSummaryRecords = false` 仍允许顺序读取消息，但不能满足基于 Chunk 索引的查询；`AllowBufferedSort = false` 的时间排序必须拒绝不支持的回退。查询成功不等于全文件校验。

## 单独选择完整性检查

Writer CRC 选项控制存储哪些校验和，reader 选项控制是否执行对应检查。CRC 为零表示没有校验和；验证无法检查被省略的校验和。CRC 不校验载荷的 Schema 语义。除非工作负载有具体理由省略检查，否则保留默认生成，读取侧检查见[校验指南](validation.md)。

## 考虑刷新与长时间录制

`Flush()` 结束当前 Chunk，频繁调用可能增加 Chunk 和索引数量。它不完成 MCAP，也不请求持久化。结束时调用 `Complete()`，需要时另行请求文件同步。

内存包括活跃 Chunk/codec 存储、声明、文件级索引、应用队列和输出目标本身。即使 `EmitChunkIndexes = false`，上游仍为每个已完成 Chunk 保留 ChunkIndex；该开关控制输出。禁用消息索引省略每个 Chunk 的索引；禁用附件/元数据索引会停止保留对应索引。持续增长的 MemoryStream 保存录制本身。库不自动分段，也不将索引溢写到磁盘。

完成时有上游摘要克隆的瞬时成本。之后绑定保留共享原生摘要和输出；完整 `GetSummary()` 副本随摘要大小增长。访问模式适合时可使用保留摘要的游标，用完后释放游标和 writer。这些不是恒定内存保证，详见[长时间录制契约](api.md#单文件长期录制)与[内存诊断](testing.md#单文件-writer-内存诊断)。

[配置示例](../../samples/Documentation/WritingExamples.cs)验证 None/Lz4/Zstd 往返及不支持的无索引排序拒绝。这些是正确性检查，不是分配或吞吐测量。按[示例说明](usage.md#运行仓库示例)运行，然后根据自身工作负载决定是否继续调优。
