using Xunit;

namespace Fizzy.McapSharp.Tests;

public class LeaseWriteTests
{
    static McapMessageBatchLease Source(McapCompression compression)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 1024, CompressionThreads = 0 }, true))
        {
            writer.RegisterChannel(7, "a", "raw"); writer.RegisterChannel(9, "b", "raw");
            writer.WriteMessage(new(7, 1, 2, 3), []);
            writer.WriteMessage(new(9, 4, 5, 6), new byte[70000]);
            var data = new byte[90000]; Array.Fill(data, (byte)42);
            writer.WriteMessage(new(7, 7, 8, 9), data);
            writer.Complete();
        }
        using var reader = new McapBufferReader(output.ToArray());
        return reader.ReadBatchLease()!;
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void OriginalAndReplacementHeadersRoundTripAcrossOwners(McapCompression compression)
    {
        using var batch = Source(compression); Assert.Equal(3, batch.Count);
        using var output = new MemoryStream();
        McapMessageHeader[] replacements = [new(11, 20, 30, 40), new(11, 21, 31, 41), new(11, 22, 32, 42)];
        using (var writer = new McapWriter(output, new() { Compression = compression, ChunkSize = 1024, CompressionThreads = 0 }, true))
        {
            writer.RegisterChannel(7, "a", "raw"); writer.RegisterChannel(9, "b", "raw"); writer.RegisterChannel(11, "mapped", "raw");
            Assert.Equal(3, writer.WriteBatch(batch));
            Assert.Equal(3, writer.WriteBatch(batch, replacements));
            Assert.Equal(3, writer.WriteBatch(batch)); // No ownership transfer or mutation.
            writer.Complete();
        }
        using var reader = new McapBufferReader(output.ToArray());
        using var actual = reader.ReadBatchLease()!;
        Assert.Equal(9, actual.Count);
        for (int i = 0; i < 9; i++)
        {
            Assert.Equal(i is >= 3 and < 6 ? replacements[i - 3] : batch.GetHeader(i % 3), actual.GetHeader(i));
            Assert.True(batch.GetPayload(i % 3).SequenceEqual(actual.GetPayload(i)));
        }
    }

    [Fact]
    public void ManagedArgumentFailuresDoNotFailWriter()
    {
        using var output = new MemoryStream();
        using var writer = new McapWriter(output, leaveOpen: true);
        writer.RegisterChannel(7, "a", "raw"); writer.RegisterChannel(9, "b", "raw");
        using var batch = Source(McapCompression.None);
        Assert.Throws<ArgumentNullException>(() => writer.WriteBatch((McapMessageBatchLease)null!));
        Assert.Throws<ArgumentException>(() => writer.WriteBatch(batch, []));
        using var disposed = Source(McapCompression.None); disposed.Dispose();
        Assert.Throws<ObjectDisposedException>(() => writer.WriteBatch(disposed));
        Assert.Equal(3, writer.WriteBatch(batch)); writer.Complete();
    }

    [Theory]
    [InlineData(false, false)] [InlineData(false, true)] [InlineData(true, false)] [InlineData(true, true)]
    public void UnknownChannelPreflightIsAtomicAndHonorsPolicy(bool strict, bool replace)
    {
        using var batch = Source(McapCompression.None);
        using var output = new MemoryStream();
        using var writer = new McapWriter(output, new() { UseChunks = false, RecoverableErrors = strict ? McapRecoverableWriterErrors.None : McapRecoverableWriterErrors.UnknownChannelOnMessageWrite }, true);
        writer.RegisterChannel(7, "a", "raw");
        McapMessageHeader[] headers = [new(7, 0, 0, 0), new(65000, 1, 1, 1), new(7, 2, 2, 2)];
        long before = output.Length;
        var error = Assert.Throws<McapBatchWriteException>(() => { if (replace) writer.WriteBatch(batch, headers); else writer.WriteBatch(batch); });
        Assert.Equal(0, error.CompletedCount); Assert.Equal(!strict, error.CanContinueWriting); Assert.Equal(before, output.Length);
        if (strict) Assert.Throws<InvalidOperationException>(() => writer.Complete());
        else { writer.RegisterChannel(9, "b", "raw"); Assert.Equal(3, writer.WriteBatch(batch)); writer.Complete(); }
        Assert.Equal(90000, batch.GetPayload(2).Length);
    }

    [Theory]
    [InlineData(false)] [InlineData(true)]
    public void StreamFailureReportsCompletedPrefix(bool replace)
    {
        using var batch = Source(McapCompression.None);
        using var output = new FailingStream();
        using var writer = new McapWriter(output, new() { UseChunks = false }, true);
        writer.RegisterChannel(7, "a", "raw"); writer.RegisterChannel(9, "b", "raw");
        output.Limit = output.Position + 31 + 10; // Complete the empty message, then fail inside the next.
        McapMessageHeader[] headers = [batch.GetHeader(0), batch.GetHeader(1), batch.GetHeader(2)];
        var error = Assert.Throws<McapBatchWriteException>(() => { if (replace) writer.WriteBatch(batch, headers); else writer.WriteBatch(batch); });
        Assert.Equal(1, error.CompletedCount); Assert.False(error.CanContinueWriting);
        Assert.Same(output.Error, error.InnerException);
        Assert.Throws<InvalidOperationException>(() => writer.WriteBatch(batch));
        Assert.Throws<InvalidOperationException>(() => writer.Complete());
        Assert.Equal(42, batch.GetPayload(2)[0]);
    }
    sealed class FailingStream : MemoryStream
    {
        public long Limit = long.MaxValue;
        public readonly IOException Error = new("injected failure");
        public override void Write(ReadOnlySpan<byte> data) { if (Position + data.Length > Limit) throw Error; base.Write(data); }
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public unsafe void UncompressedOutputReceivesTheExactLeasePointers(McapCompression compression)
    {
        using var batch = Source(compression);
        using var output = new PointerStream();
        fixed (byte* a = batch.GetPayload(1)) fixed (byte* b = batch.GetPayload(2))
        {
            output.First = (nuint)a; output.Second = (nuint)b;
            using var writer = new McapWriter(output, new() { UseChunks = false }, true);
            writer.RegisterChannel(7, "a", "raw"); writer.RegisterChannel(9, "b", "raw");
            Assert.Equal(3, writer.WriteBatch(batch));
            writer.Complete();
            Assert.Equal(2, output.Hits);
        }
    }
    sealed class PointerStream : MemoryStream
    {
        public nuint First, Second;
        public int Hits;
        public override unsafe void Write(ReadOnlySpan<byte> data)
        {
            if (data.Length is 70000 or 90000)
            {
                fixed (byte* p = data) Assert.Equal(data.Length == 70000 ? First : Second, (nuint)p);
                Hits++;
            }
            base.Write(data);
        }
    }
}
