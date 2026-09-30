using Xunit;

namespace Fizzy.McapSharp.Tests;

public class MemoryOptimizationTests
{
    static byte[] Recording(McapCompression compression)
    {
        using var output = new MemoryStream();
        using (var w = new McapWriter(output, new() { Compression = compression, ChunkSize = 2048 }, true))
        {
            var schema = w.RegisterSchema("s", "raw", [7, 8]);
            var channel = w.RegisterChannel("t", "raw", schema, new Dictionary<string, string> { ["k"] = "v" });
            for (uint i = 0; i < 32; i++) w.WriteMessage(new(channel, i, i, 0), new byte[256]);
            w.Complete();
        }
        return output.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void ConvenienceMessagesKeepIndependentMutableResults(McapCompression compression)
    {
        using var reader = new McapBufferReader(Recording(compression));
        using var it = reader.ReadMessages().GetEnumerator();
        Assert.True(it.MoveNext()); var a = it.Current;
        a.Data[0] = 33; a.Channel.Schema!.Data[0] = 44;
        ((IDictionary<string, string>)a.Channel.Metadata)["k"] = "changed";
        Assert.True(it.MoveNext()); var b = it.Current;
        reader.Dispose();
        Assert.Equal(0, b.Data[0]); Assert.Equal(7, b.Channel.Schema!.Data[0]);
        Assert.Equal("v", b.Channel.Metadata["k"]);
        Assert.Equal(33, a.Data[0]);
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void MappedReadersMatchCopiedModes(McapCompression compression)
    {
        var bytes = Recording(compression);
        foreach (var mode in Enum.GetValues<McapBufferReadMode>())
        {
            byte[] input = bytes;
            if (mode == McapBufferReadMode.SansMagic) input = bytes[8..^8];
            if (mode == McapBufferReadMode.Chunk)
            {
                using var top = new McapBufferReader(bytes, McapBufferReadMode.Linear);
                input = top.ReadRecords().First(r => r.Opcode == 6).Data;
            }
            var path = Path.GetTempFileName();
            try
            {
                File.WriteAllBytes(path, input);
                using var mapped = McapBufferReader.OpenMapped(path, mode, options: new() { MaxOwnedInputBytes = 0 });
                using var copied = new McapBufferReader(input, mode);
                var a = mapped.ReadRecords().ToArray(); var b = copied.ReadRecords().ToArray();
                Assert.Equal(b.Length, a.Length);
                for (int i = 0; i < a.Length; i++) { Assert.Equal(b[i].Opcode, a[i].Opcode); Assert.Equal(b[i].Data, a[i].Data); }
                Assert.Equal((ulong)input.Length, mapped.GetMemoryStatistics().MappedBytes);
            }
            finally { File.Delete(path); }
        }
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void CachedRandomAccessMatchesOfficialAndDoesNotMoveCursors(McapCompression compression)
    {
        var bytes = Recording(compression);
        using var official = new McapIndexSnapshot(bytes, new() { MaxPendingBufferBytes = 0 });
        foreach (ulong budget in new ulong[] { 0, 1, 4096, 1024 * 1024 })
        {
            using var cached = new McapIndexSnapshot(bytes, new() { MaxRandomAccessCacheBytes = budget });
            var chunks = cached.GetSummary()!.ChunkIndexes;
            using var cursor = cached.OpenChunkReader(chunks[0]);
            foreach (var chunk in chunks.Reverse().Concat(chunks))
            {
                var entries = official.ReadMessageIndexes(chunk).SelectMany(x => x.Records).ToArray();
                foreach (var e in entries.Reverse().Concat(entries))
                {
                    var expected = official.SeekMessage(chunk, e);
                    var actual = cached.SeekMessage(chunk, e);
                    Assert.Equal(expected.Sequence, actual.Sequence); Assert.Equal(expected.Data, actual.Data);
                }
                // Complete caller index identity must be observed even for the same chunk offset.
                Assert.Throws<McapException>(() => cached.SeekMessage(chunk with { ChunkLength = 0 }, entries[0]));
            }
            Assert.Equal(0u, cursor.ReadMessages().First().Sequence);
        }
    }

    [Theory]
    [InlineData(2)] [InlineData(5)]
    public void RandomProbeRetryReusesTheOwnedResult(int operation)
    {
        using var snapshot = new McapIndexSnapshot(Recording(McapCompression.Zstd));
        var chunk = snapshot.GetSummary()!.ChunkIndexes[0];
        var entry = snapshot.ReadMessageIndexes(chunk)[0].Records[0];
        McapReadStatus Read(byte[] b, out ulong n) => operation == 2
            ? snapshot.SeekMessage(chunk, entry, b, out _, out n) : snapshot.ReadMessageIndexes(chunk, b, out n);
        Assert.Equal(McapReadStatus.BufferTooSmall, Read([], out var length));
        var before = snapshot.GetMemoryStatistics();
        Assert.Equal(McapReadStatus.BufferTooSmall, Read([], out _));
        Assert.Equal(before, snapshot.GetMemoryStatistics());
        Assert.Equal(McapReadStatus.Message, Read(new byte[(int)length], out _));
        var after = snapshot.GetMemoryStatistics();
        Assert.Equal(before.AllocationCount, after.AllocationCount);
        Assert.Equal(before.CopiedBytes + length, after.CopiedBytes);
        Assert.True(after.CurrentControlledBytes < before.CurrentControlledBytes);
    }

    [Theory]
    [InlineData(0)] [InlineData(1)] [InlineData(2)]
    public void ScratchBudgetBoundariesRestoreStreamPosition(int adjustment)
    {
        var bytes = Recording(McapCompression.None);
        // Header record starts immediately after magic.
        int length = checked((int)System.Buffers.Binary.BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(9)));
        using var stream = new MemoryStream(bytes);
        using var reader = McapReader.OpenMessages(stream, leaveOpen: true,
            options: new() { Memory = new() { MaxScratchBufferBytes = (ulong)(length - 1 + adjustment) } });
        long position = stream.Position;
        var output = Enumerable.Repeat((byte)99, length).ToArray();
        if (adjustment == 0)
        {
            var error = Assert.Throws<McapException>(() => reader.ReadRecordAt(8, output, out _, out _));
            Assert.Equal("ScratchBuffer", error.Details.GetProperty("resource").GetString());
            Assert.All(output, b => Assert.Equal(99, b));
            Assert.False(reader.IsComplete);
            Assert.Throws<InvalidOperationException>(() => reader.ReadNext(output, out _, out _));
        }
        else
        {
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadRecordAt(8, [], out _, out var required));
            Assert.Equal((ulong)length, required);
            Assert.Equal(McapReadStatus.Message, reader.ReadRecordAt(8, output, out _, out _));
            var allocations = reader.GetMemoryStatistics().AllocationCount;
            reader.ReadRecordAt(8, output, out _, out _);
            Assert.Equal(allocations, reader.GetMemoryStatistics().AllocationCount);
            Assert.Equal(bytes.AsSpan(17, length).ToArray(), output);
        }
        Assert.Equal(position, stream.Position);
    }

    [Fact]
    public void MappedRandomRecordNeedsNoScratchAndIndexedStreamObeysScratchBudget()
    {
        var bytes = Recording(McapCompression.Zstd); var path = Path.GetTempFileName();
        try
        {
            File.WriteAllBytes(path, bytes);
            using var mapped = new McapReader(path).OpenMessages(options: new() { Memory = new() { MaxScratchBufferBytes = 0 } });
            mapped.ReadRecordAt(8, new byte[1024], out _, out _);
            Assert.Equal(0ul, mapped.GetMemoryStatistics().CurrentControlledBytes);
            using var stream = McapReader.OpenMessages(new MemoryStream(bytes), new(),
                options: new() { Memory = new() { MaxScratchBufferBytes = 0 } });
            var error = Assert.Throws<McapException>(() => stream.ReadNext(new byte[1024], out _, out _));
            Assert.Equal("ScratchBuffer", error.Details.GetProperty("resource").GetString());
            Assert.Throws<InvalidOperationException>(() => stream.ReadNext(new byte[1024], out _, out _));
        }
        finally { File.Delete(path); }
    }
}
