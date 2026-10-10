# Fizzy.McapSharp 文档

[English](../index.md)

Fizzy.McapSharp 为 .NET 8 提供通用 MCAP 文件操作；payload 编码和时钟语义由应用定义。

## 入门与使用

- [使用任务与可运行示例](usage.md)：写入、验证和读取第一个文件。
- [API 契约](api.md)：选择[消息所有权](api.md#选择消息所有权)和[访问模式](api.md#选择访问模式)，然后了解完成、释放、失败及分配保证。

## 开发与内部实现

- [开发指南](development.md)：构建并验证当前源码或完整包。
- [原生 ABI](native.md)：原生互操作与内存边界。
- [上游覆盖](coverage.md)：与官方 Rust API 的对应关系。
- [本地补丁](patches.md)：允许的扩展及其证据。

## 文档维护

两种语言的指南共用站点导航中的英文生成式 **API reference (English)**。成员契约查阅 API 参考，跨 API 行为查阅指南。

使用[文档维护](documentation.md)构建站点并维护翻译，按照[发布 SOP](release.md)归档和发布文档。
