using Xunit;

namespace Fizzy.McapSharp.Tests;

public class InputReservationTests
{
    [Theory]
    [InlineData(McapCompression.None, 1)] [InlineData(McapCompression.None, 8)] [InlineData(McapCompression.None, 32)]
    [InlineData(McapCompression.Lz4, 1)] [InlineData(McapCompression.Lz4, 8)] [InlineData(McapCompression.Lz4, 32)]
    [InlineData(McapCompression.Zstd, 1)] [InlineData(McapCompression.Zstd, 8)] [InlineData(McapCompression.Zstd, 32)]
    public void LargeRecordsShortReadsRetriesAndStrictValidation(McapCompression compression, int mib)
    {
        var payload = new byte[mib * 1024 * 1024]; new Random(13).NextBytes(payload);
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { Compression = compression, CompressionThreads = 0, ChunkSize = 1024 }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            writer.WriteMessage(new(channel, 0, 0, 0), []);
            writer.WriteMessage(new(channel, 1, 1, 1), payload);
            writer.WriteMessage(new(channel, 2, 2, 2), [42]);
            writer.Complete();
        }
        using var input = new ShortStream(storage.ToArray());
        using var reader = McapFileReader.OpenMessages(input, leaveOpen: true, options: McapReaderOptions.Strict);
        Assert.Equal(McapReadStatus.Success, reader.ReadNext([], out _, out var empty)); Assert.Equal(0UL, empty);
        for (int i = 0; i < 3; i++)
        {
            Assert.Equal(McapReadStatus.BufferTooSmall, reader.ReadNext([], out var header, out var size));
            Assert.Equal(1U, header.Sequence); Assert.Equal((ulong)payload.Length, size);
        }
        var target = new byte[payload.Length];
        Assert.Equal(McapReadStatus.Success, reader.ReadNext(target, out _, out _)); Assert.Equal(payload, target);
        Assert.Equal(McapReadStatus.Success, reader.ReadNext(target, out var last, out var length));
        Assert.Equal(2U, last.Sequence); Assert.Equal(1UL, length); Assert.Equal(42, target[0]);
        Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext([], out _, out _)); Assert.True(reader.IsFullyValidated);
        Assert.Equal(McapReadStatus.EndOfStream, reader.ReadNext([], out _, out _));
    }
    sealed class ShortStream(byte[] data) : Stream
    {
        readonly MemoryStream inner = new(data, false);
        public override bool CanRead => true;
        public override bool CanSeek => true;
        public override bool CanWrite => false;
        public override long Length => inner.Length;
        public override long Position { get => inner.Position; set => inner.Position = value; }
        public override int Read(Span<byte> buffer)
        {
            Assert.InRange(buffer.Length, 0, 65536);
            return inner.Read(buffer[..Math.Min(buffer.Length, 4096)]);
        }
        public override int Read(byte[] buffer, int offset, int count) => throw new Exception("Array fallback");
        public override long Seek(long offset, SeekOrigin origin) => inner.Seek(offset, origin);
        public override void Flush() { }
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        protected override void Dispose(bool disposing) { if (disposing) inner.Dispose(); base.Dispose(disposing); }
    }
}
