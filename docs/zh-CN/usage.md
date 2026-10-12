# 入门：写入和读取第一个文件

[English](../en/usage.md) | 简体中文

面向通过 NuGet 使用本库的 .NET 开发者。本教程从创建项目开始，依次完成写入、读取、过滤和可选的完整校验，无需克隆本仓库或安装 Rust。需要 .NET 8 SDK，并在 Windows x64 或受支持的 glibc Linux x64/ARM64 环境运行。

## 创建项目

```powershell
dotnet new console -n McapQuickStart -f net8.0
cd McapQuickStart
dotnet add package Fizzy.McapSharp
```

将 `Program.cs` 替换为下面的写入代码，再按顺序追加后续代码块。示例使用 C# 12 和项目默认的隐式 using；执行 `dotnet run` 即可运行。

## 写入两条消息

```csharp
using Fizzy.McapSharp;
using System.Text;

var path = Path.Combine(Path.GetTempPath(), $"sample-{Guid.NewGuid():N}.mcap");
using (var writer = new McapWriter(path))
{
    var channel = writer.RegisterChannel("/sample", "json");
    writer.WriteToKnownChannel(new McapMessageHeader(channel, 0, 100, 100), "{\"value\":1}"u8);
    writer.WriteToKnownChannel(new McapMessageHeader(channel, 1, 50, 50), "{\"value\":2}"u8);
    writer.Complete();
}
```

Channel 将主题 `/sample` 与 `json` 编码关联；此示例不注册 Schema。消息载荷是 UTF-8 JSON，库本身不解析 JSON。`LogTime` 和 `PublishTime` 的单位均为纳秒，时钟基准由应用决定。这里故意先写入时间 100，再写入时间 50，以便观察读取顺序。

路径构造函数仅创建新文件，已有文件会导致失败。必须显式调用 `Complete()`；退出 `using` 只释放资源，不隐式完成文件。文件持久化是另外的请求，规则见 [API 契约](api.md#格式完成与文件持久化)。

## 读取消息

```csharp
var reader = new McapReaderFactory(path);
foreach (var message in reader.ReadMessages())
    Console.WriteLine($"{message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

输出按文件顺序排列：

```text
100: {"value":1}
50: {"value":2}
```

`McapReaderFactory` 本身不持有需要释放的输入资源；每次枚举拥有并释放独立读取会话。`ReadMessages()` 返回的每条消息包含独立的数据副本，适合普通应用代码。

<a id="按主题和时间过滤"></a>

## 可选：按主题和时间过滤

```csharp
foreach (var message in reader.ReadMessages(new()
{
    Topic = "/sample",
    StartTime = 50,
    EndTime = 100
}))
    Console.WriteLine($"{message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

输出只有 `50: {"value":2}`。时间过滤针对 `LogTime`，范围为 `[StartTime, EndTime)`：包含 50，不包含 100。增加过滤条件仍保持文件顺序；需要时间排序或强制索引时，见[查询与随机访问](queries.md)。

## 按需校验和清理

```csharp
ulong recordCount = reader.Validate();
Console.WriteLine($"Validated {recordCount} records.");
```

这是可选的独立全文件扫描，可在处理消息前执行。返回值是记录数，不是消息数；格式或完整性错误会抛异常。普通读取或查询成功不能替代完整校验，校验也不解释业务载荷。详细范围及有效前缀恢复见 [API 契约](api.md#校验与恢复)。

完成所有操作后可追加 `File.Delete(path);` 删除示例文件；应用代码应按自己的保留策略管理文件，并处理操作或清理异常。

## 运行仓库示例

[教程源码](../../samples/Documentation/QuickStartExamples.cs)验证上述输出并清理临时文件。其他写入、读取、转发和校验示例见[示例入口](../../samples/Documentation/Program.cs)；构建与运行方式见[开发指南](development.md#运行仓库示例)。

## 下一步

- [写入文件](writing.md)：保存多个主题的消息，添加元数据和附件。
- [读取消息](reading.md)：读取消息内容、声明及其他记录。
- [文档首页](index.md)：按需求查找查询、校验和进阶指南。
