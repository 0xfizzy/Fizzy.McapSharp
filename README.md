# Fizzy.McapSharp

.NET 8 bindings to the official Rust MCAP implementation, independent of robotics or business payloads. Version **0.1.0** initially supports **Windows x64**. The NuGet package includes the native runtime.

```csharp
using Fizzy.McapSharp;

using (var writer = new McapWriter("new-recording.mcap"))
{
    var channel = writer.RegisterChannel("/sample", "json");
    writer.WriteMessage(channel, 100, 100, 0, "{}"u8);
    writer.Complete();
}

var reader = new McapReader("new-recording.mcap");
reader.Validate();
foreach (var message in reader.ReadMessages(new() { Topic = "/sample" }))
    Console.WriteLine(message.LogTime);
```

Build and test: `./scripts/Build.ps1 -Test -Pack` (Rust 1.98.1, .NET 8 SDK and Visual C++ build tools).

[API and ownership](docs/api.md) · [Native ABI](docs/native.md) · [Release configuration](docs/releasing.md)
