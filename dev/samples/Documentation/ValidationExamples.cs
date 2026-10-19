using System.Buffers.Binary;
using Fizzy.McapSharp;

internal static class ValidationExamples
{
    internal static void Run(string directory)
    {
        var completePath = Path.Combine(directory, "complete.mcap");
        using (var writer = new McapWriter(completePath, new() { UseChunks = false }))
        {
            var channel = writer.RegisterChannel("/sample", "raw");
            writer.WriteToKnownChannel(new(channel, 0, 1, 1), [42]);
            writer.WriteToKnownChannel(new(channel, 1, 2, 2), [43]);
            writer.Complete();
        }
        var factory = new McapReaderFactory(completePath);
        if (factory.Validate() <= 2) throw new Exception("Validation returns records, not messages.");
        using (var reader = factory.OpenMessages(options: McapReaderOptions.Strict))
        {
            reader.ValidateRemaining();
            if (!reader.IsFullyValidated) throw new Exception("Strict complete scan not validated.");
        }

        // Walk MCAP top-level framing: 8-byte magic, then opcode + u64 body length.
        // End the copy immediately after the first complete Message (opcode 0x05).
        var original = File.ReadAllBytes(completePath);
        int end = 8;
        while (true)
        {
            byte opcode = original[end];
            int length = checked((int)BinaryPrimitives.ReadUInt64LittleEndian(original.AsSpan(end + 1, 8)));
            end = checked(end + 9 + length);
            if (opcode == 0x05) break;
        }
        var truncatedPath = Path.Combine(directory, "prefix.mcap");
        File.WriteAllBytes(truncatedPath, original.AsSpan(0, end).ToArray());
        var before = File.ReadAllBytes(truncatedPath);
        var recovered = new List<McapMessage>();
        var result = new McapReaderFactory(truncatedPath).RecoverMessages(recovered.Add);
        if (result.IsFullyValidated || result.Error is null || result.RecoveredMessageCount != 1 ||
            recovered.Count != 1 || !recovered[0].Data.AsSpan().SequenceEqual(new byte[] { 42 }))
            throw new Exception("Unexpected prefix recovery result.");
        if (!File.ReadAllBytes(truncatedPath).AsSpan().SequenceEqual(before) ||
            !File.ReadAllBytes(completePath).AsSpan().SequenceEqual(original))
            throw new Exception("Recovery must not modify input files.");
    }
}
