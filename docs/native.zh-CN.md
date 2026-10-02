# 原生 ABI 与内存边界

[English](native.md) | 简体中文

.NET 使用 Cdecl P/Invoke 和 SafeHandle 调用 Rust cdylib，底层为官方 mcap 0.25.0，没有 C++ 层。[lib.rs](../native/src/lib.rs) 实现 MCAP 操作，[io.rs](../native/src/io.rs) 实现文件/Stream I/O，托管声明见 [Native.cs](../src/Native.cs)。Windows x64 加载 fizzy_mcap_native.dll，glibc Linux x64/ARM64 加载 libfizzy_mcap_native.so，托管层使用无扩展名的 fizzy_mcap_native。

## ABI 契约

`fm_abi_version()` 返回 13，托管构造函数拒绝不匹配，包括缺少 lease 批量写入的旧原生库。这不是稳定的第三方 ABI；不兼容变更必须同时更新版本检查和所有平台原生资产。

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

控制操作码：1 Schema、2 Channel、4 元数据、5 附件、6 Flush、7 Complete、8 开始附件、9 附件片段、10 结束附件、11 私有记录、12 已完成 Writer 的摘要、13 已完成文件同步。消息通过专用入口处理。

私有消息头为 24 字节：u16 channel_id、u16 reserved、u32 sequence、u64 log_time、u64 publish_time，偏移分别为 0、2、4、8、16。与上游记录逐字段转换，不依赖 Rust 记录布局。读取 EOF 时，索引扫描及排序回退的 reserved 为 1，顺序扫描为 0；托管层据此判断是否能报告完整校验。

响应为 40 字节，依次包含 JSON 指针／usize 长度、binary 指针／usize 长度、u64 value。所有支持平台的指针和 nuint 均为 64 位。状态 0 成功、1 EOF、2 缓冲不足、负值错误。读取时 value 表示所需／复制字节数，EOF 时表示扫描数量。缓冲不足不写部分结果、不消费 pending。

Writer 创建选项包含不可变位掩码 `recoverableErrors`：1 表示显式 Schema ID 无效，2 表示显式 Schema 冲突，4 表示 Channel 注册引用未知 Schema，8 表示显式 Channel 冲突，16 表示 header/payload 消息写入引用未知 Channel。省略时为 31；未知位在创建输出前被拒绝。Writer 状态 -2 保留普通结构化错误响应并允许继续使用；-1（包括 panic）表示终止。托管层先检查回调异常，再依据本次状态设置 `CanContinueWriting`，不依据异常类型放行。Reader 错误不采用恢复语义。

## 数据与生命周期

消息热路径只使用固定数据和调用方缓冲；边界上不需要 JSON、托管 payload 数组、原生结果分配或逐消息 Channel 描述序列化。Reader 内部仍可分配原生缓冲，并将原生数据复制到托管调用方内存。冷路径请求/描述使用长度限定 UTF-8 JSON，二进制数据不使用 Base64。

冷路径响应使用 Rust 所有的 JSON／binary 分配，调用方以 `fm_buffer_free` 释放；`Native.Consume` 在 finally 中释放两者。错误 JSON 包含 kind、message、details，不做固定长度截断。错误格式化也受 panic 边界保护。成功的热路径不返回自有响应缓冲。输入 Span 仅在同步调用期间固定。便利结果拥有托管副本；借用回调和 lease 按 API 生命周期契约使用。

每个会话独占一个原生 Reader。映射输入同时拥有文件；增量 sans_io::LinearReader 状态和待处理记录使用原生自有缓冲，无需延长借用迭代器生命周期。Stream 增量读取并支持短读。索引查询直接使用官方 IndexedReader，时间排序可同时保留重叠 Chunk，不宣称完整校验；排序扫描回退会在原生内存收集匹配消息；摘要声明不足时回退顺序读取，解析部分摘要可能需要一次冷路径全扫描。

映射文件必须保持不变。Windows 拒绝普通竞争写入/删除，但之前已有的可写映射不受此保护；Linux 不强制互斥。并发截断可终止进程，超出 panic/异常边界。随机读取按源长度检查记录边界，但原生分配和解压仍需要与记录/Chunk 大小相应的内存，共享存储使用有限资源域；codec 工作区等排除项见 API 指南。

## Stream 回调与释放

回调表为 48 字节：上下文指针、Read/Write/Seek/Flush 函数指针、u32 seekable 及对齐填充。回调采用 Cdecl，返回状态，通过输出指针返回字节数/位置。托管桥接每个会话只固定一次回调上下文；SafeHandle 先释放原生句柄，再解除上下文根引用并关闭拥有的 Stream。

回调在发起线程同步执行。托管异常在回调内捕获并返回失败，退出原生边界后重新抛出原异常。禁止重入，也禁止同一 Stream 被两个会话同时占用。Seek 偏移相对捕获的 MCAP 起点；非寻址 Writer 只允许查询当前位置，采用上游 disable_seeking(true) 缓冲。

Writer 操作串行化，Writer 状态 -2 表示按配置放行的、经核验的修改前拒绝；-1 及其他原生错误为终止失败。Complete 调用上游 finish 后普通刷新输出，并保留输出至释放。操作码 13 要求完成成功且输出为原生 File，然后调用 sync_all。FileStream 持久化在托管 Writer 锁和重入保护下调用 Flush(true)，不改变 callback 布局。同步失败终止 Writer。Drop 使用上游 into_inner，避免隐式完成；free 捕获析构 panic，释放后回调指针不再可用。

Writer 完成时在上游 `finish` 后立即提取输出，释放上游 writer 缓存的 summary 和声明。绑定层只保留 `Arc<Summary>`；操作 12 按需将字段和索引条目编码到最终 UTF-8 响应，中间 JSON 存储最多覆盖当前记录／统计对象，不保留完整 JSON 树。Summary 游标通过 Arc 独立于 writer 保留数据。上游完成时的 summary 克隆和文件级索引积累不变；这是绑定层所有权优化，不是上游内存上限或 ABI 变更。

## 错误边界与验证

可失败原生入口捕获 panic 并转换成错误响应。分配器 abort 和外部非法指针无法转换成托管异常；调用方必须传入有效缓冲及本 ABI 创建的句柄。Rust 编译期断言和托管测试验证支持平台上的布局大小及偏移。

[公共 API](api.zh-CN.md) 区分完整校验、索引查询和原始记录。[构建与分配验收](development.zh-CN.md) 分别验证托管分配和格式互操作；托管零分配不等于原生零分配。


## 扩展操作族

`fm_channel_prepare/free` 保存不可变 Channel／Schema 快照。`fm_writer_full_message` 使用该描述符与同步借用 payload 调用官方写入。`fm_operation_prepare/free` 保存冷路径解析后的控制信息；`fm_writer_prepared` 复用描述符，避免重复托管序列化。`fm_writer_private` 接收标量标志和 Span。

`fm_engine_open/next/feed/free` 封装官方线性、摘要和索引 Sans-I/O 状态。事件为 56 字节：u32 kind、u32 opcode、u64 length、u64 offset、u32 seek origin、u32 reserved、24 字节消息头。kind 0–5 对应 End、Read、Seek、Record、Message、ReadChunk；Current/End 定位偏移保留有符号补码。输入请求等待供给，记录/消息在缓冲不足时保持待取。`fm_engine_index_control` 支持索引插入及长度限制更新；`fm_engine_summary` 和 `fm_summary_records` 输出自有摘要或原生记录游标。

`fm_buffer_reader_*` 拥有输入副本和配置匹配官方切片入口的 Sans-I/O 解析器，推进时只保留一条待交付记录及已遇到声明，不跨调用保留借用迭代器。由于上游 for_chunk 私有，Chunk 适配器通过公开解析器输入合成记录前缀。`fm_snapshot_*` 保存输入副本或映射及官方摘要，随机消息读取通过私有句柄保留共享完整 chunk 存储。`fm_snapshot_call` 同步接收标准 MCAP 索引记录体的指针/长度，以及消息索引的 LogTime 和 offset 两个标量；解析传入索引，不在摘要中查找替代索引。托管桥接使用有界栈缓冲或临时非托管内存编码索引，包括 UTF-8 字符串和通道偏移映射。`fm_reader_record_into`、`fm_parse_record`、`fm_footer`、`fm_chunk_offset` 提供缓冲区或标量操作。`fm_snapshot_chunk_reader` 新增独立惰性 Chunk 游标，共享不可变原生输入与摘要，快照释放不影响已创建游标。Reader open 状态 3 表示禁止所需缓存排序，映射为 `NotSupportedException`。句柄均由私有 SafeHandle 管理，游标成功读取不分配响应缓冲。

异步读取由 .NET ReadAsync 驱动线性引擎，等待期间仅保留托管 Memory；复用完成源和 continuation，避免逐操作分配。资源 SafeHandle 在释放 Stream 所有权前释放解析器，遗漏 Dispose 时也可终结。取消终止会话；释放前必须消费在途操作。


## 稳定存储与批次

`fm_writer_batch` 接收 24 字节 Header、8 字节 offset/length 范围、共享 payload 和独立的已完成前缀输出。`fm_read_batch`／`fm_visit_messages` 使用 40 字节 Progress（四个 u64、两个 u32）。预检先于写入；推进后的失败不回滚。

`fm_writer_lease_batch` 接收 writer 与 lease 句柄、可选的 header 指针及 usize 数量、usize 已完成前缀输出和普通 Response。header 为 null 且数量为零时使用 lease 原有 header；否则数量必须匹配 lease 的消息数。写入前检查全部目标 Channel，直接读取保留批次的 payload 切片，不构造 payload 或描述符数组。托管端在整个调用期间显式持有 lease 的 SafeHandle 引用。Header 布局和批次错误语义不变。

`fm_reader_owned`、`fm_buffer_reader_owned`、`fm_snapshot_message_owned` 使用两个指针的 Sink（context、Cdecl 回调）。visitor 返回 1 表示正常停止，负值失败；托管异常不跨 FFI 展开。每次退出清除 sink。公开借用 Span 在回调返回时失效，自有交付创建独立副本。

`fm_read_lease`、`fm_engine_lease_step` 与 `fm_lease_get/retain/free` 使用 SharedBytes 和普通批次描述符。存储所有权使映射及文件在 reader 释放后仍有效；Span 使用不得与释放并发。`fm_engine_input_buffer/complete` 让 Stream.ReadAsync 通过 MemoryManager 直接填充解析器存储，期间不得推进或销毁引擎。

pending 保存共享字节或合成记录体。Buffer reader 保留完整 Message body，交付时才选择 payload 范围，支持 record/message 重试切换。indexed Stream 将读入缓冲的所有权转入解析器，避免对未压缩 chunk 再复制一次；短读使用 read_exact。缓存和排序仅执行 API 描述的局部限制。

同步线性 feeding 通过 try_insert 预留 parser 当前完整需求，每次最多向该存储读取 64 KiB。顺序 Stream 读取、摘要回退扫描及验证复用同一 helper。异步直接填充也预留完整需求，只暴露所选 I/O 传输区间。异步 lease 交付采用可复用的显式状态机和缓存的 I/O continuation；仍允许 lease 结果及控制对象分配。非空批次仍在下一次输入请求时返回。

回退排序使用绑定层 arena，保存 header、文件序号和共享范围。固定大小的待处理组跟踪连续 owned backing 的身份与选中字节，不建立全文件 owner 字典。组结束时，可将稀疏选中数据一次复制到按消息划分的不可变段，再发布结果；映射及其他外部 owner 不紧凑化。`SharedBytes::shares_backing` 是本地只读所有权查询，紧凑化决策和分配仍在绑定层执行。现有逻辑排序检查不变，新旧存储重叠不计入该额度。失败终止构造，不发布部分会话；ABI 不变。

官方格式状态机、codec 及默认写入行为由固定的 mcap crate 提供；共享存储、批次预检所需 channel 查询以及禁止销毁时隐式完成属于[本地补丁](patches.zh-CN.md)。SafeHandle、托管副本、局部缓存／排序及异步 I/O 是绑定层行为。
