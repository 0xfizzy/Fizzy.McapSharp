using Xunit;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp.Tests;
public sealed class NativeInteropTests
{
    [Theory]
    [InlineData(McapCompression.None, false)]
    [InlineData(McapCompression.Lz4, false)]
    [InlineData(McapCompression.Zstd, false)]
    [InlineData(McapCompression.None, true)]
    [InlineData(McapCompression.Lz4, true)]
    [InlineData(McapCompression.Zstd, true)]
    public void StreamsAndExtendedFeatures(McapCompression compression, bool seekable)
    {
        using var storage = new MemoryStream();
        storage.Write(new byte[17]);
        using (var stream = new TestStream(storage, seekable))
        {
            using var w = new McapWriter(stream, new() { Compression = compression, ChunkSize = 64 }, true);
            Assert.Equal((ushort)7, w.RegisterSchema(7, "schema", "raw", new byte[] { 1, 2 }));
            Assert.Equal((ushort)9, w.RegisterChannel(9, "topic", "raw", 7));
            for (uint i = 0; i < 20; i++)
                w.WriteMessage(new McapMessageHeader(9, i, i, i + 1), new byte[] { 3, 4, 5 });
            w.WritePrivateRecord(0x80, [8, 9], true);
            w.WriteMetadata("session", new Dictionary<string, string> { { "k", "v" } });
            w.StartAttachment("a", "raw", 1, 2, 3);
            w.WriteAttachmentBytes([1]);
            Assert.Throws<InvalidOperationException>(() => w.Flush());
            w.WriteAttachmentBytes([2, 3]);
            w.FinishAttachment();
            w.Complete();
            w.Complete();
            Assert.Equal(20ul, w.GetSummary().Statistics!.MessageCount);
        }

        storage.Position = 17;
        using (var stream = new TestStream(storage, seekable, 3))
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            if (seekable)
            {
                var summary = r.GetSummary()!;
                Assert.Equal(20ul, summary.Statistics!.MessageCount);
                Assert.NotEmpty(r.ReadMessageIndexes(summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0)));
                Assert.Equal((byte)6, r.ReadChunk(summary.ChunkIndexes.First(c => c.MessageIndexOffsets.Count > 0)).Opcode);
            }
            else
                Assert.Throws<NotSupportedException>(() => r.GetSummary());
            var small = new byte[]
            {
                99,
                99
            };
            Assert.Equal(McapReadStatus.BufferTooSmall, r.ReadNext(small, out var h, out var required));
            Assert.Equal(3ul, required);
            Assert.Equal(new byte[] { 99, 99 }, small);
            Assert.Equal(0u, h.Sequence);
            var buffer = new byte[3];
            for (uint i = 0; i < 20; i++)
            {
                Assert.Equal(McapReadStatus.Message, r.ReadNext(buffer, out h, out required));
                Assert.Equal(i, h.Sequence);
                Assert.Equal(new byte[] { 3, 4, 5 }, buffer);
            }

            Assert.Equal("schema", r.GetChannel(9).Schema!.Name);
            Assert.Equal(McapReadStatus.EndOfStream, r.ReadNext(buffer, out _, out _));
            Assert.True(r.IsComplete);
            Assert.Equal(20ul, r.GetSummary()!.Statistics!.MessageCount);
        }

        storage.Position = 17;
        using (var r = McapReader.OpenRecords(new TestStream(storage, false, 7)))
        {
            var records = r.ReadRecords().ToArray();
            Assert.Contains(records, x => x.Opcode == 0x80 && x.Data.SequenceEqual(new byte[] { 8, 9 }));
            Assert.Single(records, x => x.Opcode == 9);
        }
    }

    [Fact]
    public void StreamFailuresAndOwnership()
    {
        var storage = new MemoryStream();
        var stream = new TestStream(storage, true);
        var w = new McapWriter(stream, new() { UseChunks = false }, true);
        var c = w.RegisterChannel("t", "raw");
        Assert.Throws<InvalidOperationException>(() => McapReader.OpenMessages(stream));
        stream.ThrowOnWrite = true;
        Assert.Throws<IOException>(() => w.WriteMessage(new McapMessageHeader(c, 0, 1, 1), [1]));
        Assert.Throws<InvalidOperationException>(() => w.Complete());
        w.Dispose();
        Assert.False(stream.Disposed);
        using var storage2 = new MemoryStream();
        var stream2 = new TestStream(storage2, false);
        using (var writer = new McapWriter(stream2))
        {
            writer.RegisterChannel("t", "raw");
            stream2.ThrowOnFlush = true;
            Assert.Throws<IOException>(() => writer.Complete());
        }

        Assert.True(stream2.Disposed);
    }

    [Fact]
    public void ReentryIsRejected()
    {
        using var storage = new MemoryStream();
        using var stream = new TestStream(storage, true);
        using var w = new McapWriter(stream, new() { UseChunks = false }, true);
        var c = w.RegisterChannel("t", "raw");
        stream.OnWrite = () => w.Dispose();
        Assert.Throws<InvalidOperationException>(() => w.WriteMessage(new McapMessageHeader(c, 0, 1, 1), [1]));
    }

    [Fact]
    public void AttachmentMismatchIsTerminal()
    {
        using var s = new MemoryStream();
        using var w = new McapWriter(s, leaveOpen: true);
        w.StartAttachment("x", "raw", 0, 0, 5);
        w.WriteAttachmentBytes([1]);
        Assert.Throws<McapException>(() => w.FinishAttachment());
        Assert.Throws<InvalidOperationException>(() => w.Complete());
    }

    [Fact]
    public void NoImplicitCompletionAndTruncatedRecovery()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(new TestStream(s, false), new() { UseChunks = false }, true))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1]);
            w.Flush();
        }

        s.Position = 0;
        using var r = McapReader.OpenMessages(new TestStream(s, false, 2));
        int count = 0;
        var result = r.RecoverMessages(_ => count++);
        Assert.Equal(1, count);
        Assert.False(result.IsComplete);
    }

    [Fact]
    public void PartialSummaryFallsBackToSequentialDeclarations()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, new() { RepeatSchemas = false, RepeatChannels = true }, true))
        {
            var schema = w.RegisterSchema("s", "raw", [1]);
            var c = w.RegisterChannel("t", "raw", schema);
            w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1]);
            w.Complete();
        }

        s.Position = 0;
        using (var r = McapReader.OpenMessages(s, new() { Topic = "t", Order = McapReadOrder.File }, true, McapReaderOptions.Strict))
        {
            Assert.Empty(r.GetSummary()!.SchemaIds);
            Assert.Single(r.ReadMessages());
            Assert.True(r.IsComplete);
        }

        s.Position = 0;
        using (var r = McapReader.OpenRecords(s, McapRecordMode.TopLevel, true))
        {
            Assert.Contains(r.ReadRecords(), x => x.Opcode == 6);
            Assert.Empty(r.GetSummary()!.SchemaIds);
        }
    }

    [Fact]
    public void SummaryDisabledAndPrivateRecords()
    {
        using var stream = new MemoryStream();
        using (var w = new McapWriter(stream, new() { EmitSummaryOffsets = false, EmitStatistics = false, EmitChunkIndexes = false, EmitMessageIndexes = false, EmitAttachmentIndexes = false, EmitMetadataIndexes = false, RepeatChannels = false, RepeatSchemas = false, ChunkSize = null, UseChunks = false, CalculateChunkCrcs = false, CalculateDataSectionCrc = false, CalculateSummarySectionCrc = false, CalculateAttachmentCrcs = false }, true))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 0, 1, 1), [5]);
            w.WritePrivateRecord(0x81, [6]);
            w.Complete();
        }

        stream.Position = 0;
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            Assert.Null(r.GetSummary());
            Assert.Single(r.ReadMessages());
            Assert.Null(r.GetSummary());
        }

        stream.Position = 0;
        using (var r = McapReader.OpenRecords(stream, McapRecordMode.TopLevel, true))
        {
            Assert.Contains(r.ReadRecords(), x => x.Opcode == 0x81 && x.Data[0] == 6);
            Assert.False(r.IsComplete);
        }
    }

    [Fact]
    public void IndexedQueryDoesNotValidateUntouchedChunks()
    {
        using var stream = new MemoryStream();
        using (var w = new McapWriter(stream, new() { ChunkSize = 32 }, true))
        {
            var c = w.RegisterChannel("t", "raw");
            for (uint i = 0; i < 8; i++)
                w.WriteMessage(new McapMessageHeader(c, i, i * 100, i * 100), new byte[100]);
            w.Complete();
        }

        McapChunkIndex damaged;
        stream.Position = 0;
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            damaged = r.GetSummary()!.ChunkIndexes.Last(c => c.MessageIndexOffsets.Count > 0);
        }

        var data = stream.GetBuffer();
        int at = checked((int)(damaged.ChunkStartOffset + damaged.ChunkLength - 1));
        data[at] ^= 1;
        stream.Position = 0;
        using (var r = McapReader.OpenMessages(stream, new() { StartTime = 0, EndTime = 100 }, true))
        {
            Assert.Single(r.ReadMessages());
            Assert.False(r.IsComplete);
        }

        stream.Position = 0;
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            Assert.Throws<McapException>(() => r.ValidateRemaining());
        }
    }

    [Fact]
    public void SpecifiedIdsAndInputOwnership()
    {
        using var stream = new MemoryStream();
        var payload = new byte[]
        {
            1,
            2,
            3
        };
        using (var w = new McapWriter(stream, leaveOpen: true))
        {
            var s = w.RegisterSchema(42, "s", "raw", payload);
            Assert.Equal(s, w.RegisterSchema("s", "raw", payload));
            var c = w.RegisterChannel(0, "t", "raw", s);
            w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), payload);
            Array.Fill(payload, (byte)99);
            w.Complete();
        }

        stream.Position = 0;
        McapMessage message;
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            message = Assert.Single(r.ReadMessages());
        }

        Assert.Equal(new byte[] { 1, 2, 3 }, message.Data);
        Assert.Equal(new byte[] { 1, 2, 3 }, message.Channel.Schema!.Data);
        using var other = new MemoryStream();
        using var writer = new McapWriter(other, new() { RecoverableErrors = McapRecoverableWriterErrors.None }, leaveOpen: true);
        writer.RegisterSchema(1, "s", "raw", []);
        Assert.Throws<McapException>(() => writer.RegisterSchema(1, "different", "raw", []));
        Assert.Throws<InvalidOperationException>(() => writer.Complete());
    }

    [Fact]
    public void StreamReadFailureAndCallbackRecoveryException()
    {
        using var stream = new MemoryStream();
        using (var w = new McapWriter(stream, leaveOpen: true))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1]);
            w.Complete();
        }

        stream.Position = 0;
        using (var faulty = new TestStream(stream, false)
        {
            ThrowOnRead = true
        }

        )
        using (var r = McapReader.OpenMessages(faulty, leaveOpen: true))
        {
            Assert.Throws<IOException>(() => r.ReadNext(new byte[1], out _, out _));
            Assert.Throws<InvalidOperationException>(() => r.ReadNext(new byte[1], out _, out _));
        }

        stream.Position = 0;
        using (var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict))
        {
            Assert.Throws<ApplicationException>(() => r.RecoverMessages(_ => throw new ApplicationException()));
        }
    }

    [Fact]
    public void PendingReadSurvivesRandomAccessAndDoubleDispose()
    {
        using var stream = new MemoryStream();
        using (var w = new McapWriter(stream, leaveOpen: true))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 5, 7, 6), [1, 2]);
            w.Complete();
            w.Dispose();
        }

        stream.Position = 0;
        var r = McapReader.OpenMessages(stream, leaveOpen: true, options: McapReaderOptions.Strict);
        Assert.Equal(McapReadStatus.BufferTooSmall, r.ReadNext([], out _, out _));
        var summary = r.GetSummary()!;
        Assert.Equal((byte)6, r.ReadChunk(summary.ChunkIndexes[0]).Opcode);
        var buffer = new byte[2];
        Assert.Equal(McapReadStatus.Message, r.ReadNext(buffer, out var h, out _));
        Assert.Equal(5u, h.Sequence);
        Assert.Equal(new byte[] { 1, 2 }, buffer);
        r.Dispose();
        r.Dispose();
        Assert.Throws<ObjectDisposedException>(() => r.ReadNext(buffer, out _, out _));
    }

    [Fact]
    public void NullStreamsAndInvalidModesAreRejected()
    {
        Assert.Throws<ArgumentNullException>(() => new McapWriter((Stream)null !));
        Assert.Throws<ArgumentNullException>(() => McapReader.OpenMessages(null !));
        Assert.Throws<ArgumentNullException>(() => McapReader.OpenRecords(null !));
        using var s = new MemoryStream();
        Assert.Throws<ArgumentOutOfRangeException>(() => McapReader.OpenRecords(s, (McapRecordMode)99));
    }

    [Fact]
    public void AbiLayouts()
    {
        Assert.Equal(5u, Native.fm_abi_version());
        Assert.Equal(56, Marshal.SizeOf<Native.ReadEvent>());
        Assert.Equal(32, Marshal.OffsetOf<Native.ReadEvent>(nameof(Native.ReadEvent.Header)).ToInt32());
        Assert.Equal(24, Marshal.SizeOf<Native.NativeHeader>());
        Assert.Equal(8, Marshal.OffsetOf<Native.NativeHeader>(nameof(Native.NativeHeader.LogTime)).ToInt32());
        Assert.Equal(40, Marshal.SizeOf<Native.Result>());
        Assert.Equal(48, Marshal.SizeOf<Native.Callbacks>());
        void Offsets<T>(string[] fields, int[] expected) where T : struct =>
            Assert.Equal(expected, fields.Select(name => Marshal.OffsetOf<T>(name).ToInt32()));
        Offsets<Native.Result>(["Json", "JsonLength", "Data", "DataLength", "Value"], [0, 8, 16, 24, 32]);
        Offsets<Native.NativeHeader>(["ChannelId", "Reserved", "Sequence", "LogTime", "PublishTime"], [0, 2, 4, 8, 16]);
        Offsets<Native.Callbacks>(["Context", "Read", "Write", "Seek", "Flush", "Seekable"], [0, 8, 16, 24, 32, 40]);
        Offsets<Native.ReadEvent>(["Kind", "Opcode", "Length", "Offset", "Origin", "Reserved", "Header"], [0, 4, 8, 16, 24, 28, 32]);
    }

    [Fact]
    public void EmptyAndBufferRetry()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, leaveOpen: true))
        {
            var c = w.RegisterChannel("t", "raw");
            w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), []);
            w.WriteMessage(new McapMessageHeader(c, 1, 1, 1), new byte[100000]);
            w.Complete();
        }

        s.Position = 0;
        using var r = McapReader.OpenMessages(s, leaveOpen: true);
        Assert.Equal(McapReadStatus.Message, r.ReadNext([], out _, out var n));
        Assert.Equal(0ul, n);
        for (int i = 0; i < 3; i++)
            Assert.Equal(McapReadStatus.BufferTooSmall, r.ReadNext([], out _, out n));
        Assert.Equal(100000ul, n);
        Assert.Equal(McapReadStatus.Message, r.ReadNext(new byte[100000], out var h, out _));
        Assert.Equal(1u, h.Sequence);
        Assert.Equal(McapReadStatus.EndOfStream, r.ReadNext([], out _, out _));
    }
}

internal sealed class TestStream(Stream inner, bool seekable, int maxRead = int.MaxValue) : Stream
{
    public bool Disposed, ThrowOnWrite, ThrowOnFlush, ThrowOnRead;
    public Action? OnWrite;
    public override bool CanRead => inner.CanRead;
    public override bool CanWrite => inner.CanWrite;
    public override bool CanSeek => seekable;
    public override long Length => seekable ? inner.Length : throw new NotSupportedException();

    public override long Position
    {
        get => seekable ? inner.Position : throw new NotSupportedException();
        set
        {
            if (!seekable)
                throw new NotSupportedException();
            inner.Position = value;
        }
    }

    public override int Read(Span<byte> buffer)
    {
        if (ThrowOnRead)
            throw new IOException("Injected read failure");
        return inner.Read(buffer[..Math.Min(maxRead, buffer.Length)]);
    }

    public override int Read(byte[] b, int o, int n) => Read(b.AsSpan(o, n));
    public override void Write(ReadOnlySpan<byte> buffer)
    {
        OnWrite?.Invoke();
        if (ThrowOnWrite)
            throw new IOException("Injected write failure");
        inner.Write(buffer);
    }

    public override void Write(byte[] b, int o, int n) => Write(b.AsSpan(o, n));
    public override long Seek(long n, SeekOrigin o) => seekable ? inner.Seek(n, o) : throw new NotSupportedException();
    public override void SetLength(long n) => inner.SetLength(n);
    public override void Flush()
    {
        if (ThrowOnFlush)
            throw new IOException("Injected flush failure");
        inner.Flush();
    }

    protected override void Dispose(bool disposing)
    {
        Disposed = true;
        base.Dispose(disposing);
    }
}
