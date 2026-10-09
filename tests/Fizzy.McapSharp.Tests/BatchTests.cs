using System.Runtime.InteropServices;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class BatchTests
{
    [Fact]
    public void BatchIoFailureReportsOnlyCompletedPrefixAndTerminatesWriter()
    {
        using var stream=new LimitedStream();
        using var writer=new McapWriter(stream,new(){UseChunks=false,Compression=McapCompression.None},true);
        var c=writer.RegisterChannel("t","raw");
        stream.Limit=stream.Position+32+10;
        var error=Assert.Throws<McapBatchWriteException>(()=>writer.WriteBatch([new(c,1,1,0),new(c,2,2,0),new(c,3,3,0)],[42],[new(0,1),new(0,1),new(0,1)]));
        Assert.Equal(1,error.CompletedCount);Assert.False(error.CanContinueWriting);
        Assert.Throws<InvalidOperationException>(()=>writer.WriteMessage(new(c,4,4,0),[42]));
        Assert.Throws<InvalidOperationException>(()=>writer.Complete());
    }
    sealed class LimitedStream:MemoryStream
    {
        public long Limit=long.MaxValue;
        public override void Write(ReadOnlySpan<byte> data) {if(Position+data.Length>Limit) throw new IOException("injected write failure");base.Write(data);}
        public override void Write(byte[] buffer,int offset,int count) {if(Position+count>Limit) throw new IOException("injected write failure");base.Write(buffer,offset,count);}
    }

    [Fact]
    public void LayoutsMatchNative()
    {
        Assert.Equal(24, Marshal.SizeOf<McapMessageHeader>());
        Assert.Equal(8, Marshal.SizeOf<McapPayloadRange>());
        Assert.Equal(40, Marshal.SizeOf<Native.BatchProgress>());
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void BatchesPreservePayloadAndRetry(McapCompression compression)
    {
        using var storage = new MemoryStream();
        using (var writer = new McapWriter(storage, new() { Compression = compression, ChunkSize = 32 }, true))
        {
            var c = writer.RegisterChannel("t", "raw");
            McapMessageHeader[] headers = [new(c, 1, 1, 0), new(c, 2, 2, 0), new(c, 3, 3, 0)];
            Assert.Equal(3, writer.WriteBatch(headers, [1, 2, 3, 4, 5], [new(0, 2), new(2, 3), new(0, 0)]));
            writer.Complete();
        }
        using var buffer = new McapReadCursor(storage.ToArray());
        storage.Position = 0;
        using var session = McapReaderFactory.OpenMessages(storage, options: McapReaderOptions.Strict, leaveOpen: true);
        foreach (bool useSession in new[] { false, true })
        {
            var headers = new McapMessageHeader[4]; var ranges = new McapPayloadRange[4];
            var payload = new byte[2];
            var first = useSession ? session.ReadBatch(headers, ranges, payload) : buffer.ReadBatch(headers, ranges, payload);
            Assert.Equal(new McapBatchReadResult(1, 2, McapBatchStopReason.BufferTooSmall, 3), first);
            Assert.Equal(new byte[] { 1, 2 }, payload);
            Assert.Equal(1U, headers[0].Sequence);
            payload = new byte[3];
            var second = useSession ? session.ReadBatch(headers, ranges, payload) : buffer.ReadBatch(headers, ranges, payload);
            Assert.Equal(new McapBatchReadResult(2, 3, McapBatchStopReason.EndOfStream, 0), second);
            Assert.Equal(new byte[] { 3, 4, 5 }, payload);
            Assert.Equal(new McapPayloadRange(3, 0), ranges[1]);
        }
        Assert.True(session.IsFullyValidated);
    }

    [Fact]
    public void InvalidChannelRejectsEntireBatch()
    {
        using var storage = new MemoryStream();
        using var writer = new McapWriter(storage, leaveOpen: true);
        var c = writer.RegisterChannel("t", "raw");
        var error = Assert.Throws<McapBatchWriteException>(() => writer.WriteBatch([new(c, 0, 0, 0), new(65000, 1, 1, 0)], [7], [new(0, 1), new(0, 1)]));
        Assert.Equal(0, error.CompletedCount);
        Assert.True(error.CanContinueWriting);
        writer.WriteMessage(new(c, 9, 9, 0), [9]); writer.Complete(); writer.Dispose();
        storage.Position = 0;
        using var reader = McapReaderFactory.OpenMessages(storage, leaveOpen: true);
        Assert.Equal(9U, Assert.Single(reader.ReadMessages()).Sequence);
    }

    [Theory]
    [InlineData(false)] [InlineData(true)]
    public void BorrowedCallbackStopsAndRejectsReentry(bool useSession)
    {
        var data = DeliveryOptimizationTests.Recording(McapCompression.Zstd);
        using var session = McapReaderFactory.OpenMessages(new MemoryStream(data));
        using var buffer = new McapReadCursor(data);
        int calls = 0;
        bool Accept(in McapMessageHeader h, ReadOnlySpan<byte> payload)
        {
            Assert.Equal(70000, payload.Length); calls++;
            if (useSession)
            {
                Assert.Throws<InvalidOperationException>(() => session.Dispose());
                Assert.Throws<InvalidOperationException>(() => session.ReadNext([], out _, out _));
            }
            else
            {
                Assert.Throws<InvalidOperationException>(() => buffer.Dispose());
                Assert.Throws<InvalidOperationException>(() => buffer.ReadNext([], out _, out _));
            }
            return false;
        }
        var first = useSession ? session.VisitMessages(Accept) : buffer.VisitMessages(Accept);
        Assert.Equal(new McapVisitResult(1, McapBatchStopReason.VisitorStopped), first);
        bool Last(in McapMessageHeader h, ReadOnlySpan<byte> payload) { Assert.Empty(payload.ToArray()); calls++; return true; }
        var last = useSession ? session.VisitMessages(Last) : buffer.VisitMessages(Last);
        Assert.Equal(new McapVisitResult(1, McapBatchStopReason.EndOfStream), last);
        Assert.Equal(2, calls);
    }

    [Fact]
    public void CallbackExceptionIsRethrownAndTerminatesSession()
    {
        using var reader = McapReaderFactory.OpenMessages(new MemoryStream(DeliveryOptimizationTests.Recording(McapCompression.None)));
        var expected = new ApplicationException("callback");
        bool Fail(in McapMessageHeader h, ReadOnlySpan<byte> p) => throw expected;
        Assert.Same(expected, Assert.Throws<ApplicationException>(() => reader.VisitMessages(Fail)));
        Assert.Throws<InvalidOperationException>(() => reader.ReadNext([], out _, out _));
    }
}
