# 原生 ABI 与内存边界

[English](native.md) | 简体中文

.NET 使用 Cdecl P/Invoke 和 SafeHandle 调用 Rust cdylib，底层为官方 mcap 0.25.0，没有 C++ 层。[lib.rs](../native/src/lib.rs) 实现 MCAP 操作，[io.rs](../native/src/io.rs) 实现文件/Stream I/O，托管声明见 [Native.cs](../src/Native.cs)。Windows x64 加载 fizzy_mcap_native.dll，glibc Linux x64/ARM64 加载 libfizzy_mcap_native.so，托管层使用无扩展名的 fizzy_mcap_native。

## ABI 契约

`fm_abi_version()` 返回 5，托管构造函数拒绝不匹配。这不是稳定的第三方 ABI；不兼容变更必须同时更新版本检查和所有平台原生资产。

| 入口 | 用途 |
| --- | --- |
| `fm_writer_open` / `fm_writer_free` | 创建/释放 Writer，可传 Stream 回调。 |
| `fm_writer_message` | 固定消息头加借用输入指针/长度，同步消费。 |
| `fm_writer_call` | 冷路径控制操作，以及附件/私有记录数据。 |
| `fm_reader_open` / `fm_reader_free` | 创建/释放独立读取会话。 |
| `fm_reader_next` | 将消息 payload 或原始记录 body 复制到调用方缓冲。 |
| `fm_reader_describe` | 复制已知 Schema/Channel 描述，Schema 数据使用独立二进制缓冲。 |
| `fm_reader_summary` / `fm_reader_record_at` | 获取摘要或随机原始记录，不消费待处理的顺序记录。 |
| `fm_validate` | 完整扫描映射文件。 |
| `fm_buffer_free` | 释放 Rust 拥有的响应缓冲。 |

控制操作码：1 Schema、2 Channel、4 元数据、5 附件、6 Flush、7 Complete、8 开始附件、9 附件片段、10 结束附件、11 私有记录、12 已完成 Writer 的摘要。消息通过专用入口处理。

私有消息头为 24 字节：u16 channel_id、u16 reserved、u32 sequence、u64 log_time、u64 publish_time，偏移分别为 0、2、4、8、16。与上游记录逐字段转换，不依赖 Rust 记录布局。读取 EOF 时，索引扫描及排序回退的 reserved 为 1，顺序扫描为 0；托管层据此判断是否能报告完整校验。

40 字节响应包含 JSON 指针/usize 长度、二进制指针/usize 长度和 u64 标量。支持的目标上指针及 C# nuint 均为 64 位。状态 0 成功、1 EOF、2 缓冲不足、负值错误。读取响应标量为所需/已复制长度，EOF 时为扫描计数。容量不足不修改目标缓冲，也不消费待处理记录。

Writer 创建选项包含不可变位掩码 `recoverableErrors`：1 表示显式 Schema ID 无效，2 表示显式 Schema 冲突，4 表示 Channel 注册引用未知 Schema，8 表示显式 Channel 冲突，16 表示 header/payload 消息写入引用未知 Channel。省略时为 31；未知位在创建输出前被拒绝。Writer 状态 -2 保留普通结构化错误响应并允许继续使用；-1（包括 panic）表示终止。托管层先检查回调异常，再依据本次状态设置 `CanContinueWriting`，不依据异常类型放行。Reader 错误不采用恢复语义。

## 数据与生命周期

消息热路径只使用固定数据和调用方缓冲；边界上不需要 JSON、托管 payload 数组、原生结果分配或逐消息 Channel 描述序列化。Reader 内部仍可分配原生缓冲，并将原生数据复制到托管调用方内存。冷路径请求/描述使用长度限定 UTF-8 JSON，二进制数据不使用 Base64。

非空冷路径响应缓冲属于 Rust。Native.Consume 在 finally 中释放两个缓冲，包括错误路径；错误使用含 kind、message、details 的 UTF-8 JSON；panic 回退文本按 Binding 错误处理。成功热路径不返回需要释放的响应缓冲。输入 span 只在同步调用期间固定，原生代码不保留它。公共便利记录持有托管副本，不公开指针或原生借用视图。

每个会话独占一个原生 Reader。映射输入同时拥有文件；增量 sans_io::LinearReader 状态和待处理记录使用原生自有缓冲，无需延长借用迭代器生命周期。Stream 增量读取并支持短读。索引查询直接使用官方 IndexedReader，时间排序可同时保留重叠 Chunk，不宣称完整校验；排序扫描回退会在原生内存收集匹配消息；摘要声明不足时回退顺序读取，解析部分摘要可能需要一次冷路径全扫描。

映射文件必须保持不变。Windows 拒绝普通竞争写入/删除，但之前已有的可写映射不受此保护；Linux 不强制互斥。并发截断可终止进程，超出 panic/异常边界。随机读取按源长度检查记录边界，但原生分配和解压仍需要与记录/Chunk 大小相应的内存，没有统一配额。

## Stream 回调与释放

回调表为 48 字节：上下文指针、Read/Write/Seek/Flush 函数指针、u32 seekable 及对齐填充。回调采用 Cdecl，返回状态，通过输出指针返回字节数/位置。托管桥接每个会话只固定一次回调上下文；SafeHandle 先释放原生句柄，再解除上下文根引用并关闭拥有的 Stream。

回调在发起线程同步执行。托管异常在回调内捕获并返回失败，退出原生边界后重新抛出原异常。禁止重入，也禁止同一 Stream 被两个会话同时占用。Seek 偏移相对捕获的 MCAP 起点；非寻址 Writer 只允许查询当前位置，采用上游 disable_seeking(true) 缓冲。

Writer 操作串行化，Writer 状态 -2 表示按配置放行的、经核验的修改前拒绝；-1 及其他原生错误为终止失败。Complete 调用上游 finish 后执行文件同步或 Stream Flush。Drop 使用上游 into_inner，避免隐式完成；free 捕获析构 panic，释放后回调指针不再可用。

## 错误边界与验证

可失败原生入口捕获 panic 并转换成错误响应。分配器 abort 和外部非法指针无法转换成托管异常；调用方必须传入有效缓冲及本 ABI 创建的句柄。Rust 编译期断言和托管测试验证支持平台上的布局大小及偏移。

[公共 API](api.zh-CN.md) 区分完整校验、索引查询和原始记录。[构建与分配验收](development.zh-CN.md) 分别验证托管分配和格式互操作；托管零分配不等于原生零分配。


## 扩展操作族

`fm_channel_prepare/free` 保存不可变原生 Channel/Schema 快照，`fm_writer_full_message` 使用该描述和同步借用的 payload 直接调用上游 write。`fm_operation_prepare/free` 保存冷路径控制描述，`fm_writer_prepared` 复用描述，避免托管序列化；`fm_writer_private` 使用标量标志和 Span。

`fm_engine_open/next/feed/free` 封装官方线性、摘要和索引 Sans-I/O 状态。事件为 56 字节：u32 kind、u32 opcode、u64 length、u64 offset、u32 seek origin、u32 reserved、24 字节消息头。kind 0–5 对应 End、Read、Seek、Record、Message、ReadChunk；Current/End 定位偏移保留有符号补码。输入请求等待供给，记录/消息在缓冲不足时保持待取。`fm_engine_index_control` 支持索引插入及长度限制更新；`fm_engine_summary` 和 `fm_summary_records` 输出自有摘要或原生记录游标。

`fm_buffer_reader_*` 拥有输入副本和配置匹配官方切片入口的 Sans-I/O 解析器，推进时只保留一条待交付记录及已遇到声明，不跨调用保留借用迭代器。由于上游 for_chunk 私有，Chunk 适配器通过公开解析器输入合成记录前缀。`fm_snapshot_*` 保存源文件副本和官方摘要，随机操作直接调用上游而不跨 FFI 借用。`fm_snapshot_call` 同步接收标准 MCAP 索引记录体的指针/长度，以及消息索引的 LogTime 和 offset 两个标量；解析传入索引，不在摘要中查找替代索引。托管桥接使用有界栈缓冲或临时非托管内存编码索引，包括 UTF-8 字符串和通道偏移映射。`fm_reader_record_into`、`fm_parse_record`、`fm_footer`、`fm_chunk_offset` 提供缓冲区或标量操作。`fm_snapshot_chunk_reader` 新增独立惰性 Chunk 游标，共享不可变原生输入与摘要，快照释放不影响已创建游标。Reader open 状态 3 表示禁止所需缓存排序，映射为 `NotSupportedException`。句柄均由私有 SafeHandle 管理，游标成功读取不分配响应缓冲。

异步读取由 .NET ReadAsync 驱动线性引擎，等待期间仅保留托管 Memory；复用完成源和 continuation，避免逐操作分配。资源 SafeHandle 在释放 Stream 所有权前释放解析器，遗漏 Dispose 时也可终结。取消终止会话；释放前必须消费在途操作。
