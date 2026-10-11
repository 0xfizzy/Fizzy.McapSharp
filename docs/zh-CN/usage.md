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
