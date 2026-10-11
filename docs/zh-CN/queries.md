# 查询与随机访问

[English](../queries.md) | 简体中文

面向已完成[入门教程](usage.md)、需要过滤、时间排序或局部文件访问的应用开发者。先选择顺序与验证范围，再选择结果的交付方式。下方示例沿用教程中的 `path`。

## 选择执行路径

| 需求 | 入口 | 行为 |
| --- | --- | --- |
| 按文件顺序扫描，可带过滤 | `ReadMessages` / `OpenMessages` | 顺序读取；增加过滤条件不会排序 |
| 按时间读取，允许必要的回退 | `ReadQueryMessages` / `QueryMessages` | 默认按日志时间升序；索引无法满足时可能收集匹配结果 |
| 禁止全局扫描排序 | `QueryMessages` 配置 `AllowBufferedSort = false` | 请求的时间顺序需要缓冲回退时抛异常；仍可按文件顺序扫描 |
| 强制使用可用索引 | `OpenIndexedMessages` | 不回退到顺序扫描或缓冲排序 |

每个入口的省略与空选项含义相同。主题精确匹配，时间过滤针对 `LogTime` 的 `[StartTime, EndTime)` 区间。升序和降序均不规定相同时间戳之间的顺序。索引前提与失败行为见[查询契约](api.md#选择查询路径)。

## 过滤并保持文件顺序

```csharp
var factory = new McapReaderFactory(path);
foreach (var message in factory.ReadMessages(new() { Topic = "/sample" }))
    Console.WriteLine(message.LogTime);
```

教程文件会先输出 100，再输出 50。不需要全局时间顺序时，顺序扫描适合增量处理。

## 显式请求时间顺序

```csharp
using var query = factory.QueryMessages(new()
{
    Topic = "/sample",
    Order = McapReadOrder.LogTime,
    AllowBufferedSort = false
});
foreach (var message in query.ReadMessages())
    Console.WriteLine(message.LogTime);
```

带索引的教程文件会先输出 50，再输出 100。索引无法提供此顺序时，打开操作会在收集匹配结果前抛出 `NotSupportedException`。若允许全局收集并排序，使用默认的 `AllowBufferedSort = true`。`MaxBufferedSortBytes` 约束局部收集额度，不是全部保留内存的上限，详见[局部限制](api.md#绑定层性能与局部限制)。

若必须使用索引，而不只是禁止排序回退：

```csharp
using var indexed = factory.OpenIndexedMessages(new() { Topic = "/sample" });
foreach (var message in indexed.ReadMessages())
    Console.WriteLine(message.LogTime);
```

索引查询成功不等于全文件验证。索引损坏和 I/O 错误可能直接失败，不触发回退。

## 重复访问时减少重复工作

索引查询会话与 `McapIndexedReader` 用途不同：后者持有复制或映射的输入，支持离散定位和 Chunk 游标。

| 访问模式 | 选择 | 复用范围 |
| --- | --- | --- |
| 按已知偏移读取原始记录 | `factory.OpenRecordAccessor()` | 可寻址输入，不复制整个输入 |
| 同一 Chunk 中的多条消息 | `McapIndexedReader.OpenChunkMessages` | 同一个游标的遍历 |
| 一组已知消息位置 | `SeekMessages` | 一次调用内合并 Chunk 加载 |
| 跨调用反复访问 | 为同一个索引读取器设置缓存额度 | Chunk 保留到驱逐或释放 |

文件能在所有依赖游标和租约存活期间保持不变时，使用 `McapIndexedReader.OpenMapped(path)`。需要独立完整输入副本时，选择 `factory.CreateCopiedIndexedReader()`，其内存随文件大小增长。两种读取器都需要释放；子 Chunk 游标与保留的租约持有各自的存储引用。

单条定位也可能加载并验证整个 Chunk。预准备索引可减少描述符工作，但不消除缓存未命中时的解压。先观察缓存命中与 Chunk 加载次数，再调整额度，详见[随机访问契约](api.md#完整-chunk-随机访问)。

下一步独立选择[消息所有权](ownership.md)。是否需要另外进行全文件校验，参见 [API 契约](api.md#校验与恢复)。

本页代码在[读取示例](../../samples/Documentation/ReadingExamples.cs)中运行，执行方法见[教程](usage.md#运行仓库示例)。
