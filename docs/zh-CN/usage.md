# 使用任务与可运行示例

[English](../usage.md) | 简体中文

[完整示例](../../samples/Documentation/Program.cs)无需硬件，会删除临时录制文件。在仓库根目录运行 `./scripts/Test-Samples.ps1`，先构建本机 native 库，再针对当前源码执行。验证已有完整包使用 `./scripts/Test-Samples.ps1 -PackageDirectory artifacts/packages`。

## 写入与验证

注册 Channel，写入 header/payload，并显式调用 `Complete()`。路径构造函数拒绝已有文件；`Dispose()` 只释放资源，不完成格式。`FlushToDisk()` 是独立的持久化请求。`Validate()` 检查整个文件的格式完整性，不解释业务 payload。详细规则见 [API 契约](api.md)。

## 调用方缓冲区

示例从空缓冲区开始，遇到 `BufferTooSmall` 后按返回大小分配，再重试同一条待处理记录。成功后应在复用缓冲区前处理数据；EOF 不是错误。缓冲区扩容和初始化不属于预热后的零托管分配保证，该保证也不代表零 native 分配或零复制。

## 查询路径

按 topic／时间过滤且保持录制顺序时，调用 `OpenMessages(new() { Topic = "/sample" })`；需要时间范围时添加 `StartTime`／`EndTime`，分别是 LogTime 的包含下界和不包含上界。文件顺序不需要设置回退策略。省略与空过滤条件都使用文件顺序；增加 Topic 或 Topics 不改变顺序。

需要时间顺序但不接受全局扫描排序时，使用 `QueryMessages(new() { Order = McapReadOrder.LogTime, AllowBufferedSort = false })`。查询无法使用索引时，打开会话会在收集匹配消息前抛出 `NotSupportedException`。默认的 `AllowBufferedSort = true` 则可能在返回会话前收集全部匹配结果。`ReverseLogTime` 使用相同回退策略；两个时间顺序都不指定相同时间戳之间的顺序。

必须使用索引时，调用文件工厂的 `OpenIndexedMessages()`；仅禁止缓冲排序仍允许文件顺序扫描。索引读取也可能缓冲 Chunk，查询成功不能替代全文件验证。示例检查文件顺序与默认 query 顺序、禁用回退时受支持的时间排序，以及无索引源上的拒绝行为。详见[查询路径与局部额度](api.md#选择查询路径)。

## 借用与保留

visitor 同步消费 span，不得保留 span 或重入 reader。batch lease 在 reader 释放后仍保留不可变存储；所有 span 使用结束前必须保持 lease 存活。示例声明目标 Channel 后同步转发保留的 batch。lease 可能保留整个 Chunk 或映射，payload 长度不代表总保留内存上限。

需要独立可变结果且不想管理 lease 时，选择 `ReadMessages()`。若只需长期保留少量 payload，可在[释放 lease 前复制所需字节](api.md#为保留结果选择存储)。调用方缓冲即使走零托管分配路径，也会复制 payload。回调避免最终交付复制，但不能保留 span；lease 避免该复制，却会保留 backing 存储。这些是不同的成本，不是低总内存保证。选择交付方式前，先核对[适用的 reader 类型](api.md#选择消息所有权)。

## Stream 与异步所有权

调用方需要保留 Stream 时使用 `leaveOpen: true`；session 拥有 Stream 期间不得从外部访问。每次异步操作必须 await 并消费结果，再开始下一次操作或释放。取消会终止 reader，不是重置协议。示例始终使用McapAsyncMessageReader，并释放每个返回的 lease。

完整生命周期、重试及错误边界以 [API 契约](api.md)为准。示例验证行为，不提供吞吐量测量；生产应用负责 payload 协议及应用错误策略。

## 驱动 Sans-I/O 协议

仅在应用需要自行执行 I/O 时使用这些协议。[可运行源码](../../samples/Documentation/SansIoExamples.cs) 会执行全部三个循环。下方辅助方法假设完整 MCAP 从源位置零开始；存在前缀时，需要在物理位置与 MCAP 起点之间转换。示例预期包含两条消息。任何循环到达 End 都不单独证明全文件验证成功。

回应待处理请求后再获取下一事件。SupplyInput 允许短读，空输入表示 EOF，且只能回应 Read。InsertChunkData 要求完整的请求压缩数据范围，不包含 Chunk 头，因此先完成短读拼接再调用。BufferTooSmall 保留待交付输出供重试。错误回应和解析失败会终止 parser；应释放它，不重试任意失败。

### 线性记录

目标缓冲区接收原始记录体。BufferTooSmall 时扩容；需要类型化记录时显式解析。

```csharp
    static void ReadLinear(Stream input)
    {
        using var reader = new McapSansIoLinearReader();
        byte[] io = new byte[4096], record = [];
        while (true)
        {
            var status = reader.NextEvent(record, out var item);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                record = new byte[checked((int)item.Length)];
                continue;
            }
            if (item.Kind == McapLinearEventKind.Read)
            {
                int count = input.Read(io, 0, (int)Math.Min((ulong)io.Length, item.Length));
                reader.SupplyInput(io.AsSpan(0, count)); // A short read is valid; zero signals EOF.
            }
            else
            {
                // Direct linear events are raw: parse explicitly when interpretation is needed.
                _ = McapRecords.Parse(item.Opcode, record.AsSpan(0, checked((int)item.Length)));
            }
        }
    }

```

### 摘要请求

Summary 只有 Read/Seek/End，无输出缓冲区。Begin 使用 AbsolutePosition，Current/End 使用 RelativeOffset。返回已完成的 reader，调用者负责释放。

```csharp
    static McapSansIoSummaryReader ReadSummary(Stream input)
    {
        var reader = new McapSansIoSummaryReader();
        try
        {
            byte[] io = new byte[4096];
            while (reader.NextEvent(out var item) != McapReadStatus.EndOfStream)
            {
                if (item.Kind == McapSummaryEventKind.Seek)
                {
                    var seek = item.Seek;
                    long offset = seek.Origin == SeekOrigin.Begin
                        ? checked((long)seek.AbsolutePosition) : seek.RelativeOffset;
                    reader.NotifySeeked(checked((ulong)input.Seek(offset, seek.Origin)));
                }
                else
                {
                    int count = input.Read(io, 0, (int)Math.Min((ulong)io.Length, item.Length));
                    reader.SupplyInput(io.AsSpan(0, count));
                }
            }
            return reader; // The caller owns the completed parser.
        }
        catch (Exception operation)
        {
            try { reader.Dispose(); }
            catch (Exception cleanup) { throw new AggregateException(operation, cleanup); }
            throw;
        }
    }

```

### 索引消息

先调用 `using var summary = ReadSummary(input)`，再用 `using var indexed = summary.CreateIndexed()` 创建索引 parser，调用 `ReadIndexed(input, indexed)`。它唯一的输入事件为 ReadChunk，消息输出使用调用者缓冲区。

```csharp
    static void ReadIndexed(Stream input, McapSansIoIndexedReader reader)
    {
        byte[] chunk = [], payload = [];
        int messages = 0;
        while (true)
        {
            var status = reader.NextEvent(payload, out var item);
            if (status == McapReadStatus.EndOfStream) break;
            if (status == McapReadStatus.BufferTooSmall)
            {
                payload = new byte[checked((int)item.Length)];
                continue;
            }
            if (item.Kind == McapIndexedEventKind.ReadChunk)
            {
                int length = checked((int)item.Length);
                if (chunk.Length < length) chunk = new byte[length];
                input.Position = checked((long)item.Offset);
                input.ReadExactly(chunk.AsSpan(0, length)); // Complete short reads before submission.
                reader.InsertChunkData(item.Offset, chunk.AsSpan(0, length));
            }
            else
            {
                // Use item.Header and payload[..Length] before reusing the destination.
                messages++;
            }
        }
        if (messages != 2) throw new Exception("Indexed protocol example lost messages.");
    }
```
