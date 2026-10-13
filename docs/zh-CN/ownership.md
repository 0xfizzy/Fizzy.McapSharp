# 消息所有权、缓冲与批次转发

<a id="消息所有权与内存取舍"></a>

[English](../en/ownership.md) | 简体中文

面向需要决定何时复用写入输入、读取结果应保持多久有效的应用。读取先从自有结果开始；需要复用缓冲、同步处理或保留共享存储时，再选择其他交付形式。访问顺序是[独立选择](queries.md)。

## 写入输入与缓冲复用

Writer 在每次调用返回前消费载荷输入。调用结束后可以复用调用方数组或 span；不要在调用期间修改输入。`WriteToKnownChannel` 适合已注册通道和复用缓冲；完整自有消息适合已有对象；prepared 描述快照控制数据，复用时不重复准备。精确分配保证见 [API 契约](api.md#绑定层性能与局部限制)。

输入所有权不因写入而转移。Lease 转发也要求在同步写入结束前保持 lease 有效。批次若已经引用多个底层存储，可直接转发，无需拼接载荷。普通 caller-buffer 读取仍复制到调用方缓冲；便利自有结果仍是独立副本。写入形式选择见[写入指南](writing.md)。

## 按生命周期选择

| 需求 | 选择 | 主要成本或责任 |
| --- | --- | --- |
| 独立保留或修改结果 | `ReadMessages()` | 分配独立结果对象，复制载荷和可变声明 |
| 复用应用存储 | `ReadNext(buffer, ...)` | 复制到调用方存储；容量不足时扩容 |
| 返回前处理完字节 | `ReadNext(visitor)` / `VisitMessages` | 借用的 span 在回调返回时失效；禁止重入读取器 |
| 保留或转发共享字节 | `ReadBatchLease` | 租约控制对象可能分配；所有持有者释放后才能回收底层存储 |

借用是临时访问，不转移所有权。存储租约（lease）保留不可变底层存储，可能覆盖整个 Chunk 或映射。两者都不保证内存只与选中载荷的字节数成正比。选择交付形式前，先核对[读取器能力与契约](api.md#选择消息所有权)。

## 复用调用方缓冲

以下循环沿用[教程](usage.md)中的 `path`：

```csharp
using var session = new McapReaderFactory(path).OpenMessages();
byte[] buffer = [];
while (true)
{
    var status = session.ReadNext(buffer, out var header, out var length);
    if (status == McapReadStatus.EndOfStream) break;
    if (status == McapReadStatus.BufferTooSmall)
    {
        buffer = new byte[checked((int)length)];
        continue;
    }
    Console.WriteLine($"{header.LogTime}: {System.Text.Encoding.UTF8.GetString(buffer, 0, checked((int)length))}");
}
```

容量不足时，同一条消息保持待交付状态，供下一次重试。复用缓冲前先处理成功读取的字节；空消息是成功结果，不是 EOF。库的调用方缓冲路径在预热后产生 0 B 托管分配，但示例中的扩容和输出格式化仍会分配，也仍有载荷复制。详见[缓冲区契约](api.md#使用可复用缓冲读取消息)。

## 借用并立即处理

```csharp
using var borrowed = new McapReaderFactory(path).OpenMessages();
borrowed.VisitMessages((in McapMessageHeader header, ReadOnlySpan<byte> payload) =>
{
    Console.WriteLine($"{header.LogTime}: {payload.Length} bytes");
    return true;
});
```

仅在回调内使用 span。不得从回调读取、查询或释放同一个读取器。返回 false 会在当前消息后正常停止。快照回调对索引读取器及预准备索引有额外限制，详见[借用契约](api.md#借用批次与-lease)。

## 将读取批次转发到新录制

先读取源通道描述，在目标 writer 注册对应 Schema 和 Channel，建立源 Channel ID 到目标 ID 的映射。对每条消息替换头部中的 ChannelId，保持其他字段不变；载荷仍来自原 lease：

```csharp
var headers = new McapMessageHeader[batch.Count];
for (int i = 0; i < batch.Count; i++)
{
    var header = batch.GetHeader(i);
    headers[i] = header with { ChannelId = ids[header.ChannelId] };
}
writer.WriteBatch(batch, headers);
```

此片段要求 `batch` 在调用期间有效，`ids` 包含所有源通道且目标声明已注册。ID 相同时可用 `WriteBatch(batch)`。同步调用不转移所有权，批次不具备原子性，不重放整个失败批次。读取器可以先释放，独立保留的 lease 继续保有载荷；不要并发访问和释放 lease。[完整转发示例](../../samples/Documentation/RecordingExamples.cs)验证带 Schema 和无 Schema 通道的 ID 映射及往返载荷。

## 只保留应用需要的数据

短流水线可在处理期间保持批次租约存活，处理结束后释放。租约在读取器推进、释放及缓存驱逐后仍然有效。若少量载荷需要保留更久，可以显式复制：

```csharp
using var retained = new McapReaderFactory(path).OpenMessages();
byte[]? savedPayload = null;
using (var batch = retained.ReadBatchLease(1))
{
    if (batch is not null)
    {
        savedPayload = new byte[batch.GetPayload(0).Length];
        batch.CopyTo(0, savedPayload);
    }
}
// savedPayload is independent of the lease and reader.
```

其他租约、解析器或缓存仍引用的存储，不会因这次复制而释放。限制在途工作时，除批次数量外，还应考虑底层存储大小与保留时长。不得在使用 span 时并发释放租约；跨线程转移需要调用方同步。详见[保留契约](api.md#为保留结果选择存储)。

异步流水线请继续阅读 [Stream 与异步](streams-async.md)；调整内存行为前，查阅[局部限制](api.md#绑定层性能与局部限制)与[诊断方法](testing.md)。

本页代码在[读取示例](../../samples/Documentation/ReadingExamples.cs)中运行，执行方法见[教程](usage.md#运行仓库示例)。
