# Getting started: your first recording

English | [简体中文](../zh-CN/usage.md)

For .NET developers using the NuGet package. Create a project, write, read, filter and optionally validate a recording without cloning this repository or installing Rust. Use the .NET 8 SDK on Windows x64 or supported glibc Linux x64/ARM64.

## Create a project

```powershell
dotnet new console -n McapQuickStart -f net8.0
cd McapQuickStart
dotnet add package Fizzy.McapSharp
```

Replace `Program.cs` with the writing code below, then append the subsequent code blocks in order. The example uses C# 12 and the project's default implicit usings. Run it with `dotnet run`.

## Write two messages

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

The channel associates topic `/sample` with the `json` encoding; this example registers no schema. Payloads contain UTF-8 JSON, which the library does not decode. `LogTime` and `PublishTime` are nanoseconds with an application-defined clock origin. Writing time 100 before time 50 makes the reading order visible.

The path constructor only creates new files and fails if the file exists. Call `Complete()` explicitly; leaving the `using` scope releases resources without implicitly finishing the format. File persistence is a separate request; see [API contracts](api.md#completion-and-file-persistence).

## Read messages

```csharp
var reader = new McapReaderFactory(path);
foreach (var message in reader.ReadMessages())
    Console.WriteLine($"{message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

Output follows file order:

```text
100: {"value":1}
50: {"value":2}
```

`McapReaderFactory` itself owns no disposable input resources; each enumeration owns and disposes an independent session. Each message returned by `ReadMessages()` contains an independent copy of its data, suitable for ordinary application code.

<a id="filter-by-topic-and-time"></a>

## Optional: filter by topic and time

```csharp
foreach (var message in reader.ReadMessages(new()
{
    Topic = "/sample",
    StartTime = 50,
    EndTime = 100
}))
    Console.WriteLine($"{message.LogTime}: {Encoding.UTF8.GetString(message.Data)}");
```

The only output is `50: {"value":2}`. Time filtering uses `LogTime` and the interval `[StartTime, EndTime)`: 50 is included and 100 is excluded. Adding a filter preserves file order. See [queries and random access](queries.md) when you need time ordering or mandatory indexes.

## Optional validation and cleanup

```csharp
ulong recordCount = reader.Validate();
Console.WriteLine($"Validated {recordCount} records.");
```

This is an optional, separate full-file scan and can run before processing messages. It returns a record count, not a message count; format or integrity failures throw. Successful ordinary reading or querying does not replace full validation, and validation does not interpret business payloads. See [API contracts](api.md#validation-and-recovery) for validation scope and valid-prefix recovery.

Append `File.Delete(path);` after all operations to remove the example file. Application code should apply its own retention policy and handle operation or cleanup failures.

## Run repository examples

The [tutorial source](../../samples/Documentation/QuickStartExamples.cs) verifies the output and removes its temporary file. The [example entry point](../../samples/Documentation/Program.cs) also runs writing, reading, forwarding and validation examples. See [development](development.md#run-repository-examples) for build and run instructions.

## Next steps

- [Write files](writing.md): save multiple topics, metadata and attachments.
- [Read messages](reading.md): read message contents, declarations and other records.
- [Documentation home](index.md): find query, validation and advanced guides for your task.
