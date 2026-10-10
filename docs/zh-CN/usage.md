# 使用任务与可运行示例

[English](../usage.md) | 简体中文

[完整示例](../../samples/Documentation/Program.cs)无需硬件，会删除临时录制文件。在仓库根目录运行 `./scripts/Test-Samples.ps1`，先构建本机 native 库，再针对当前源码执行。验证已有完整包使用 `./scripts/Test-Samples.ps1 -PackageDirectory artifacts/packages`。

## 写入与验证

注册 Channel，写入 header/payload，并显式调用 `Complete()`。路径构造函数拒绝已有文件；`Dispose()` 只释放资源，不完成格式。`FlushToDisk()` 是独立的持久化请求。`Validate()` 检查整个文件的格式完整性，不解释业务 payload。详细规则见 [API 契约](api.md)。

## 调用方缓冲区

示例从空缓冲区开始，遇到 `BufferTooSmall` 后按返回大小分配，再重试同一条待处理记录。成功后应在复用缓冲区前处理数据；EOF 不是错误。缓冲区扩容和初始化不属于预热后的零托管分配保证，该保证也不代表零 native 分配或零复制。

## 查询路径

文件顺序适合增量扫描。`AllowBufferedSort = false` 拒绝全局扫描排序回退，但仍允许支持的索引查询。必须具备索引时使用 `OpenIndexedMessages()`。示例组合精确主题、文件顺序和禁止排序回退。索引查询成功不能替代全文件验证。

## 借用与保留

visitor 同步消费 span，不得保留 span 或重入 reader。batch lease 在 reader 释放后仍保留不可变存储；所有 span 使用结束前必须保持 lease 存活。示例声明目标 Channel 后同步转发保留的 batch。lease 可能保留整个 Chunk 或映射，payload 长度不代表总保留内存上限。

## Stream 与异步所有权

调用方需要保留 Stream 时使用 `leaveOpen: true`；session 拥有 Stream 期间不得从外部访问。每次异步操作必须 await 并消费结果，再开始下一次操作或释放。取消会终止 reader，不是重置协议。示例始终使用异步 lease 模式，并释放每个返回的 lease。

完整生命周期、重试及错误边界以 [API 契约](api.md)为准。示例验证行为，不提供吞吐量测量；生产应用负责 payload 协议及应用错误策略。
