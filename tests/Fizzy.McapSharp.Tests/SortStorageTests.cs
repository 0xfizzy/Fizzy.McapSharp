using Xunit;

namespace Fizzy.McapSharp.Tests;

public class SortStorageTests
{
    static byte[] Recording(McapCompression compression, bool chunks)
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { Compression = compression, UseChunks = chunks,
            ChunkSize = null, EmitChunkIndexes = false, CompressionThreads = 0 }, true))
        {
            var selected = writer.RegisterChannel("selected", "raw");
            var ignored = writer.RegisterChannel("ignored", "raw");
            uint sequence = 0;
            foreach (var sizes in new[] { new[] { 0, 4096, 4096 }, new[] { 300000, 300000 }, new[] { 4096 } })
            {
                foreach (int size in sizes)
                {
                    var payload = new byte[size]; Array.Fill(payload, (byte)sequence);
                    writer.WriteMessage(new(selected, sequence, sequence / 2, 0), payload);
                    sequence++;
                }
                if (sizes.Length != 2) writer.WriteMessage(new(ignored, 0, 0, 0), new byte[1024 * 1024]);
                writer.Flush();
            }
            writer.Complete();
        }
        return output.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void FallbackStoragePreservesOrderRetriesAndLeaseLifetime(McapCompression compression)
    {
        foreach (bool chunks in new[] { false, true })
        {
            var bytes = Recording(compression, chunks);
            var path = Path.GetTempFileName();
            File.WriteAllBytes(path, bytes);
            try
            {
                foreach (bool mapped in new[] { false, true })
                foreach (bool reverse in new[] { false, true })
                {
                    var query = new McapQuery { Topic = "selected", Order = reverse ? McapReadOrder.ReverseLogTime : McapReadOrder.LogTime };
                    McapReadSession Open() => mapped ? new McapFileReader(path).OpenMessages(query)
                        : McapFileReader.OpenMessages(new ShortStream(bytes), query);
                    uint[] expected = reverse ? [5, 4, 3, 2, 1, 0] : [0, 1, 2, 3, 4, 5];
                    int[] lengths = [0, 4096, 4096, 300000, 300000, 4096];
                    using (var reader = Open())
                    {
                        foreach (uint sequence in expected)
                        {
                            if (lengths[sequence] != 0)
                            {
                                for (int retry = 0; retry < 2; retry++)
                                {
                                    var sentinel = new byte[] { 91 };
                                    Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext(sentinel, out var h, out var n));
                                    Assert.Equal(sequence, h.Sequence); Assert.Equal((ulong)lengths[sequence], n);
                                    Assert.Equal(91, sentinel[0]);
                                }
                            }
                            var payload = new byte[lengths[sequence]];
                            Assert.Equal(McapReadStatus.Success, reader.ReadNext(payload, out var header, out _));
                            Assert.Equal(sequence, header.Sequence); Assert.All(payload, b => Assert.Equal((byte)sequence, b));
                        }
                        for (int eof = 0; eof < 2; eof++) Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext([], out _, out _));
                    }
                    using var leasedReader = Open();
                    using var batch = leasedReader.ReadBatchLease(10)!;
                    Assert.Equal(6, batch.Count);
                    using var retained = batch.RetainMessage(Array.IndexOf(expected, 1u));
                    Assert.Null(leasedReader.ReadBatchLease());
                    leasedReader.Dispose();
                    for (int i = 0; i < batch.Count; i++)
                    {
                        Assert.Equal(expected[i], batch.GetHeader(i).Sequence);
                        Assert.Equal(lengths[expected[i]], batch.GetPayload(i).Length);
                    }
                    batch.Dispose();
                    Assert.Equal(4096, retained.Payload.Length); Assert.Equal(1, retained.Payload[4095]);
                }
            }
            finally { File.Delete(path); }
        }
    }

    [Fact]
    public void CorruptFallbackAndSortLimitFailBeforePublishingResults()
    {
        var bytes = Recording(McapCompression.Zstd, true);
        Assert.Throws<McapException>(() => McapFileReader.OpenMessages(new ShortStream(bytes), new() { MaxBufferedSortBytes = 1 }));
        Assert.Throws<McapException>(() => McapFileReader.OpenMessages(new ShortStream(bytes[..^10]), new() { Topic = "selected" }));
    }

    sealed class ShortStream(byte[] bytes) : MemoryStream(bytes, false)
    {
        public override bool CanSeek => false;
        public override int Read(Span<byte> destination) => base.Read(destination[..Math.Min(997, destination.Length)]);
    }
}
