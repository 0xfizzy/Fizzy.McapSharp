# 校验与有效前缀恢复

[English](../en/validation.md) | 简体中文

面向需要确认录制是否完整、结构是否有效，或从不完整文件提取消息的应用。先阅读[读取指南](reading.md)；本页说明读取成功能够和不能证明什么。

## 确定检查范围

普通高层展开读取解析记录体，并检查声明 ID、冲突和引用。可选 CRC 与尾随字节检查是独立选择。到达 EOF、获取摘要或索引查询成功都不代表全文件校验。局部索引访问可能不读取文件的大部分内容。原始、顶层和 Chunk 局部能力有各自明确的较窄范围。

容器检查不验证业务载荷含义、Schema 符合性、时钟同步，也不验证生产者是否记录了全部预期消息。

## 校验完整录制

使用[写入指南](writing.md)中的 `path`：

```csharp
var factory = new McapReaderFactory(path);
ulong records = factory.Validate();
Console.WriteLine($"Validated {records} records.");
```

这会执行独立的严格全文件扫描，返回记录数而非消息数。它检查 framing、结束 magic、展开后的记录/声明有效性，以及存在的 Chunk、Attachment、Data 和 Summary CRC。CRC 为零表示没有提供校验和。格式或完整性失败会抛异常，I/O 失败也会报告给调用方。

如果应用本来就需要一个从开始即严格配置的顺序会话，可以完成该会话的检查：

```csharp
using var reader = factory.OpenMessages(options: McapReaderOptions.Strict);
reader.ValidateRemaining();
Console.WriteLine(reader.IsFullyValidated);
```

`ValidationCapability` 描述会话创建时的能力范围。只有 `StrictFullScan` 支持 `ValidateRemaining` 和会话恢复；局部索引/排序/Chunk 读取或非严格全扫描无法追溯补上跳过的检查。`IsScanComplete` 报告扫描完成，`IsFullyValidated` 报告严格全文件校验成功。详见[校验契约](api.md#校验与恢复)。

## 区分各项命名检查

`ValidateChunkCrcs` 和 `PrevalidateChunkCrcs` 选择 Chunk 校验和检查及其时机；`ValidateDataSectionCrc` 和 `ValidateSummarySectionCrc` 选择对应区段。`SkipStartMagic`、`SkipEndMagic` 和 `CheckFinishesAfterEndMagic` 控制 framing 与尾随字节。`RecordLengthLimit` 是长度限制，不是总内存预算。精确默认值和适用读取器见 [McapReaderOptions 英文参考](xref:Fizzy.McapSharp.McapReaderOptions)。

`Strict` 预设组合完整性检查。高层展开读取的声明引用和记录体校验不依赖该预设。关闭某个可选 CRC 检查不关闭这些基线检查。Writer 的校验和生成是[独立选择](writer-options.md)。

## 恢复有效消息前缀

对不完整输入使用独立的 factory 操作，不复用已经失败的读取器：

```csharp
var recovered = new List<McapMessage>();
var result = new McapReaderFactory(incompletePath).RecoverMessages(recovered.Add);
Console.WriteLine($"Recovered {result.RecoveredMessageCount} messages; validated: {result.IsFullyValidated}");
if (result.Error is { } error) Console.WriteLine(error.Message);
```

Factory 打开严格顺序读取器，交付解析失败前的独立自有消息。检查 `IsFullyValidated` 和 `Error` 两者；已交付的前缀不证明源文件完整。Chunk 内部损坏可能阻止交付该 Chunk 中的消息。未写入的字节无法恢复。列表保留所有交付消息；不需要集合时，应在每次回调中立即处理。

恢复不修改或修复源文件，不补写 footer，不重置会话，也不恢复 writer。需要将选定消息写成新录制时，显式使用新 writer 并遵守其声明与完成规则。I/O、绑定及回调异常直接传播，不变成解析错误结果。应用回调异常会终止会话，且保留原异常实例。

## 检查示例

[校验示例](../../samples/Documentation/ValidationExamples.cs)生成并校验完整的无 Chunk 录制，然后将独立副本截断在第一条完整顶层 Message 记录之后。示例断言恢复一条消息、结果不完整且源字节不变。这一确定边界避免假定压缩 Chunk 可以部分恢复。运行方法见[示例说明](usage.md#运行仓库示例)。

普通读取回到[读取指南](reading.md)；局部访问选择[查询](queries.md)，并保留其较窄的验证结论。
