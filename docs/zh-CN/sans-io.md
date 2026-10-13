# Sans-I/O 读取协议

<a id="自行驱动-sans-io"></a>

[English](../en/sans-io.md) | 简体中文

仅在应用需要自行执行 I/O 时使用这些协议。[可运行源码](../../samples/Documentation/SansIoExamples.cs) 会执行全部三个循环。下方辅助方法假设完整 MCAP 从源位置零开始；存在前缀时，需要在物理位置与 MCAP 起点之间转换。示例预期包含两条消息。任何循环到达 `End` 都不单独证明全文件验证成功。

回应待处理请求后再获取下一事件。`SupplyInput` 允许短读，空输入表示 EOF，且只能回应 `Read`。`InsertChunkData` 要求完整的请求压缩数据范围，不包含 Chunk 头，因此先完成短读拼接再调用。`BufferTooSmall` 保留待交付输出供重试。错误回应和解析失败会终止 parser；应释放它，不重试任意失败。

下方方法使用 `using Fizzy.McapSharp;` 和 .NET 项目的默认隐式 using。在线性读取时打开文件并调用 `ReadLinear(input)`；运行其他循环前重新打开文件或将流定位到起点。

这些是读取协议；写入入口见[写入指南](writing.md)，读取能力选择见[读取指南](reading.md)。

## 线性记录

目标缓冲区接收原始记录体。`BufferTooSmall` 时扩容；需要类型化记录时显式解析。

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

## 摘要请求

Summary 只有 `Read`/`Seek`/`End`，无输出缓冲区。Begin 使用 `AbsolutePosition`，Current/`End` 使用 `RelativeOffset`。返回已完成的 reader，调用者负责释放。

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

## 索引消息

先调用 `using var summary = ReadSummary(input)`，再用 `using var indexed = summary.CreateIndexed()` 创建索引 parser，调用 `ReadIndexed(input, indexed)`。它唯一的输入事件为 `ReadChunk`，消息输出使用调用者缓冲区。

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

协议能力与失败边界见 [API 契约](api.md#官方读取器直接适配)，验证方法见[测试专题](testing.md)。
