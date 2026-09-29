using System.Buffers.Binary;
using Fizzy.McapSharp;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class RobustnessTests
{
    static byte[] Small()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, new() { UseChunks = false, Compression = McapCompression.None }, true))
        { var c = w.RegisterChannel("边界", "raw"); w.WriteMessage(new McapMessageHeader(c, uint.MaxValue, ulong.MaxValue, 0), [42]); w.Complete(); }
        return s.ToArray();
    }

    [Fact]
    public void EveryTruncationFailsStrictValidationAndRecoveryNeverClaimsComplete()
    {
        var data = Small();
        for (int length = 0; length < data.Length; length++)
        {
            using var s = new MemoryStream(data, 0, length, false);
            Assert.Throws<McapException>(() => { using var r = McapReader.OpenRecords(s, options: McapReaderOptions.Strict); r.ValidateRemaining(); });
            using var recovery = new MemoryStream(data, 0, length, false);
            try { using var r = McapReader.OpenMessages(recovery, options: McapReaderOptions.Strict); Assert.False(r.RecoverMessages(_ => { }).IsComplete); }
            catch (McapException) { /* Invalid prefixes can fail before a recovery session exists. */ }
        }
    }

    [Fact]
    public void DeclaredLengthOverflowFailsBeforeAllocation()
    {
        var data = Small(); BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(9), ulong.MaxValue);
        using var s = new MemoryStream(data);
        Assert.Throws<McapException>(() => { using var r = McapReader.OpenRecords(s, options: McapReaderOptions.Strict with { RecordLengthLimit = 1024 }); r.ValidateRemaining(); });
    }

    [Theory]
    [InlineData(1)] [InlineData(2)] [InlineData(7)]
    public void ShortReadsAndExtremeMessageFieldsSurviveRetry(int size)
    {
        using var s = new FaultStream(new MemoryStream(Small()), maxRead: size);
        using var r = McapReader.OpenMessages(s);
        for (int i = 0; i < 3; i++) Assert.Equal(McapReadStatus.BufferTooSmall, r.ReadNext([], out _, out _));
        var b = new byte[1];
        Assert.Equal(McapReadStatus.Message, r.ReadNext(b, out var h, out _));
        Assert.Equal(uint.MaxValue, h.Sequence); Assert.Equal(ulong.MaxValue, h.LogTime); Assert.Equal((byte)42, b[0]);
        Assert.Equal(McapReadStatus.EndOfStream, r.ReadNext([], out _, out _));
    }

    [Theory]
    [InlineData("read")] [InlineData("write")] [InlineData("flush")]
    public void InjectedIoFailuresAreTerminalAndRespectLeaveOpen(string operation)
    {
        using var s = new FaultStream(operation == "read" ? new MemoryStream(Small()) : new MemoryStream());
        if (operation == "read")
        {
            using var r = McapReader.OpenMessages(s, leaveOpen: true);
            s.Arm(operation, 1);
            Assert.Throws<IOException>(() => r.ReadNext(new byte[1024], out _, out _));
            Assert.Throws<InvalidOperationException>(() => r.ReadNext(new byte[1024], out _, out _));
        }
        else
        {
            using var w = new McapWriter(s, new() { UseChunks = false }, true);
            var c = w.RegisterChannel("x", "raw"); s.Arm(operation, 1);
            if (operation == "write") Assert.Throws<IOException>(() => w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1, 2]));
            else Assert.Throws<IOException>(() => w.Flush());
            Assert.Throws<InvalidOperationException>(() => w.Complete());
        }
        Assert.False(s.Closed);
    }

    [Theory]
    [InlineData(1)] [InlineData(2)] [InlineData(7)]
    public void ReadFailureAtSpecifiedCallDoesNotConsumeSuccessfulResults(int call)
    {
        using var s = new FaultStream(new MemoryStream(Small()), maxRead: 1);
        using var r = McapReader.OpenMessages(s);
        s.Arm("read", call);
        Assert.Throws<IOException>(() => r.ReadNext(new byte[8], out _, out _));
        Assert.Throws<InvalidOperationException>(() => r.ReadNext(new byte[8], out _, out _));
    }

    [Theory]
    [InlineData(1)] [InlineData(9)] [InlineData(32)]
    public void PartialReadFailureAtBytePositionPreservesOriginalIoException(int position)
    {
        using var s = new FaultStream(new MemoryStream(Small()));
        using var r = McapReader.OpenMessages(s);
        s.ArmBytes(position);
        Assert.Throws<IOException>(() => r.ReadNext(new byte[8], out _, out _));
        Assert.Equal(position, s.Position);
        Assert.Throws<InvalidOperationException>(() => r.ReadNext(new byte[8], out _, out _));
    }

    [Fact]
    public void SeekFailureDuringChunkFinalizationIsTerminal()
    {
        using var s = new FaultStream(new MemoryStream());
        using var w = new McapWriter(s, new() { DisableSeeking = false }, true);
        var c = w.RegisterChannel("x", "raw");
        w.WriteMessage(new McapMessageHeader(c, 0, 0, 0), [1]);
        s.Arm("seek", 1);
        Assert.Throws<IOException>(() => w.Complete());
        Assert.Throws<InvalidOperationException>(() => w.Complete());
    }

    [Fact]
    public void IndependentSessionsAndConcurrentWriterCallsKeepTheirOwnState()
    {
        using var s = new MemoryStream();
        using (var w = new McapWriter(s, leaveOpen: true))
        {
            var c = w.RegisterChannel("parallel", "raw");
            Parallel.For(0, 128, i => w.WriteMessage(new McapMessageHeader(c, (uint)i, (ulong)i, 0), [(byte)i]));
            w.Complete();
        }
        var data = s.ToArray();
        Parallel.For(0, 8, _ =>
        {
            using var r = new McapBufferReader(data);
            Assert.Equal(Enumerable.Range(0, 128).Select(x => (uint)x), r.ReadMessages().Select(m => m.Sequence).Order());
        });
    }
}

internal sealed class FaultStream(Stream inner, int maxRead = int.MaxValue) : Stream
{
    string operation = ""; int remaining;
    long? readBytesRemaining;
    public bool Closed { get; private set; }
    public void Arm(string op, int call) { operation = op; remaining = call; }
    public void ArmBytes(long count) => readBytesRemaining = count;
    void Hit(string op) { if (operation == op && --remaining == 0) throw new IOException("Injected " + op); }
    public override bool CanRead => true;
    public override bool CanWrite => true;
    public override bool CanSeek => true;
    public override long Length => inner.Length;
    public override long Position { get => inner.Position; set => inner.Position = value; }
    public override int Read(Span<byte> b)
    {
        Hit("read");
        if (readBytesRemaining == 0) throw new IOException("Injected byte-position failure");
        int requested = (int)Math.Min(Math.Min(maxRead, b.Length), readBytesRemaining ?? int.MaxValue);
        int n = inner.Read(b[..requested]);
        if (readBytesRemaining.HasValue) readBytesRemaining -= n;
        return n;
    }
    public override int Read(byte[] b, int o, int n) => Read(b.AsSpan(o, n));
    public override void Write(ReadOnlySpan<byte> b)
    {
        // Model a legal Stream that writes a prefix and then fails.
        if (operation == "write" && remaining == 1 && b.Length > 0) inner.Write(b[..1]);
        Hit("write"); inner.Write(b);
    }
    public override void Write(byte[] b, int o, int n) => Write(b.AsSpan(o, n));
    public override long Seek(long n, SeekOrigin o) { Hit("seek"); return inner.Seek(n, o); }
    public override void Flush() { Hit("flush"); inner.Flush(); }
    public override void SetLength(long n) => inner.SetLength(n);
    protected override void Dispose(bool disposing) { Closed = true; if (disposing) inner.Dispose(); base.Dispose(disposing); }
}
