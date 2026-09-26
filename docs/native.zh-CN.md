# 原生 ABI 与内存边界

[English](native.md) | 简体中文

.NET 通过 Cdecl P/Invoke 调用 Rust `cdylib`，直接使用官方 `mcap` crate 0.25.0，没有 C++ 层。实现位于 [lib.rs](../native/src/lib.rs)，托管声明位于 [Native.cs](../src/Native.cs)。当前原生资产为 Windows MSVC x64 的 `fizzy_mcap_native.dll`。

## 版本与入口

这是私有 ABI，不是面向第三方调用者的稳定公共接口。`fm_abi_version()` 当前返回 1，托管层在创建 Reader/Writer 时检查匹配。

| 入口 | 职责 |
| --- | --- |
| `fm_writer_open` / `fm_writer_call` / `fm_writer_free` | 创建、操作、释放 Writer |
| `fm_reader_open` / `fm_reader_next` / `fm_reader_free` | 为一次枚举创建 Reader、获取记录、释放 |
| `fm_validate` | 全文件扫描校验，响应标量返回记录计数 |
| `fm_buffer_free` | 释放返回缓冲区 |

`fm_writer_call` 操作码：1 注册 Schema、2 注册 Channel、3 写消息、4 写元数据、5 写附件、6 Flush、7 Complete。变更时同步维护 Rust 分派和 C# 调用。

## 数据交换

请求控制头是长度限定的 UTF-8 JSON，二进制数据以独立指针和长度传入，并在调用内同步消费。控制 JSON 只是 ABI 实现细节，不会成为用户消息的 Schema。

响应使用 `repr(C)` / `LayoutKind.Sequential`，依次包含 JSON 指针、长度、二进制指针、长度及 `u64` 标量；长度为原生 `usize` / 托管 `nuint`。成功响应的控制头为 JSON，错误响应的同一缓冲区存放 UTF-8 错误文本。

状态 0 表示成功，1 表示 Reader EOF，负数表示错误。`Native.Consume` 复制内容后在 finally 中调用 `fm_buffer_free` 释放两个缓冲区，错误路径也必须释放。不得用托管分配器释放 Rust 缓冲区，也不得把借用的原生内存暴露给公共模型。

## 句柄、并发与释放

句柄是原生创建的不透明对象，只能由匹配的 free 函数释放一次。托管 SafeHandle 防止 P/Invoke 期间提前释放。Writer 在 C# 层加锁；单个 Reader 句柄只属于一个枚举器，不支持并发调用。

Reader 的迭代器借用固定的映射和 summary 分配。内部延长的引用生命周期只在 Reader 内有效；字段释放顺序必须保持迭代器、summary、映射、文件。移动 Reader 不能改变被借用分配的地址。返回数据会复制，不能将映射引用带出 Reader。

Windows 读取仅允许共享读取，阻止普通并发写入和删除；预先存在的可写映射无法由此排除。完整校验设置记录长度上限为映射文件长度，这不构成统一的内存配额，不能把不可信输入视为无资源风险。

Writer 释放走上游 `into_inner` 路径，避免上游 Drop 隐式完成录制。Complete 显式 finish 后执行文件 `sync_all`；失败状态不允许继续写入。

## 错误边界

可失败入口通过 `catch_unwind` 将 Rust panic 转为错误状态；句柄释放也捕获析构 panic。分配器 abort 无法转为 .NET 异常。调用方必须提供有效指针、长度和本库创建的句柄，任意外部原生指针不在支持范围内。

ABI 不兼容修改须同步版本检查；公共生命周期契约见 [api.md](api.zh-CN.md)，验证流程见 [development.md](development.zh-CN.md)。
