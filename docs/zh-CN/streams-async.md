# Stream 输入输出与异步读取

<a id="stream-与异步读取"></a>

[English](../en/streams-async.md) | 简体中文

面向提供输出/输入 Stream 或需要异步读取的应用开发者。示例沿用[教程](usage.md)中的 `path`。Stream 的所有权与[返回消息的所有权](ownership.md)需要分别选择。

## 将 Stream 交给 writer

先决定创建行为：`FileMode.CreateNew` 拒绝已有文件；`FileMode.Create` 打开时立即截断原文件。可寻址输出必须位于末尾。下面使用一个新的独立路径，并由调用方保留最终清理责任：

```csharp
using var output = new FileStream(outputPath, FileMode.CreateNew, FileAccess.Write);
using (var writer = new McapWriter(output, leaveOpen: true))
{
    var channel = writer.RegisterChannel("/sample", "raw");
    writer.WriteToKnownChannel(new(channel, 0, 0, 0), []);
    writer.Complete();
    writer.FlushToDisk();
}
// The caller still owns output; dispose it when finished.
```

Writer 调用同步消费输入，不提供异步写入会话。默认 `leaveOpen: false` 时，writer 负责释放 Stream；设为 true 时由调用方负责。Writer 持有期间，不从外部写入、重新定位或释放该 Stream。`IntoInner()` 可转移所有权，但不会隐式完成或同步文件。输出完成规则见[写入指南](writing.md)。

## 保留调用方拥有的 Stream

```csharp
using var input = File.OpenRead(path);
using (var session = McapReaderFactory.OpenMessages(input, leaveOpen: true))
{
    foreach (var message in session.ReadMessages())
        Console.WriteLine(message.LogTime);
}
input.Position = 0; // The session has released the Stream; the caller can reuse it.
```

初始位置成为 MCAP 偏移零点；从该位置到流末尾必须恰好包含一个 MCAP。会话增量读取输入。`leaveOpen` 默认为 false；仅当调用方负责后续使用与清理时设为 true。会话拥有 Stream 期间，不得从外部读取、重新定位或释放它。

## 每次等待一个操作

```csharp
await using var input = new FileStream(path, FileMode.Open, FileAccess.Read,
    FileShare.Read, 4096, FileOptions.Asynchronous);
await using (var reader = new McapAsyncMessageReader(input, leaveOpen: true))
{
    while (await reader.ReadBatchLeaseAsync() is { } lease)
    {
        using (lease)
        {
            for (int i = 0; i < lease.Count; i++)
                Console.WriteLine(lease.GetHeader(i).LogTime);
        }
    }
}
```

此段作为独立示例运行；与前一段合并时需重命名 `input`。每个 `ValueTask` 必须且只能消费一次，之后才能启动下一操作、查询声明、转移所有权或释放读取器。每个返回的租约在使用后释放。消息读取器不执行查询排序；调用方缓冲的记录读取能力由独立的 `McapAsyncRecordReader` 提供。

批次租约可能分配控制对象。异步批次已有消息时，可在下一次输入请求处提前返回，无需填满目标大小。异步 I/O 不代表任意 Stream 或调用方代码均无分配，详见[异步契约](api.md#异步记录和错误)与[批次契约](api.md#借用批次与-lease)。

## 取消与清理

需要取消时，通过读取 API 传入取消令牌。取消、解析或 I/O 失败会终止该读取器；应释放它，不要重试任意失败操作。此前已交付的租约仍有效，直到各自持有者释放。

释放前先完成并消费在途操作。读取器的 `DisposeAsync()` 执行同步释放，对拥有的 Stream 调用 `Dispose()`；它不等待在途工作，也不调用该 Stream 的 `DisposeAsync()`。示例使用 `leaveOpen: true`，将最终的异步 Stream 释放交给外层调用方作用域。

显式清理失败会报告给调用方。抛异常不代表 Stream 已可安全复用。一次性清理及所有权边界详见 [Stream 释放与转移契约](api.md#stream-会话与所有权)。

仅在应用需要自行驱动 I/O 时继续阅读 [Sans-I/O](sans-io.md)；其他情况按所选读取器查阅 [API 契约](api.md)。

本页代码在[读取示例](../../samples/Documentation/ReadingExamples.cs)中运行，执行方法见[教程](usage.md#运行仓库示例)。

输出示例在[写入示例](../../samples/Documentation/WritingExamples.cs)中验证。
