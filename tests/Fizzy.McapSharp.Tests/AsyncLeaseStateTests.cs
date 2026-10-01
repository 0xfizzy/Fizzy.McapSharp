using System.Threading.Tasks.Sources;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class AsyncLeaseStateTests
{
    static byte[] Recording()
    {
        using var output = new MemoryStream();
        using (var writer = new McapWriter(output, new() { UseChunks = false }, true))
        {
            var channel = writer.RegisterChannel("t", "raw");
            writer.WriteMessage(new(channel, 1, 1, 1), [11]);
            writer.WriteMessage(new(channel, 2, 2, 2), [22]);
            writer.Complete();
        }
        return output.ToArray();
    }

    [Theory]
    [InlineData(false)] [InlineData(true)]
    public async Task PendingIoMustBeConsumedBeforeCancellationOrFailureReleasesReader(bool fault)
    {
        using var source = new PausingStream(Recording());
        using var reader = new McapAsyncReader(source, leaveOpen: true);
        using var first = await reader.ReadBatchLeaseAsync(1);
        Assert.NotNull(first);
        source.Pause = true;
        using var cancellation = new CancellationTokenSource();
        var pending = reader.ReadBatchLeaseAsync(1, cancellationToken: cancellation.Token);
        Assert.False(pending.IsCompleted);
        Assert.Throws<InvalidOperationException>(() => pending.GetAwaiter().GetResult());
        Assert.Throws<InvalidOperationException>(() => reader.ReadBatchLeaseAsync());
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[64]));
        Assert.Throws<InvalidOperationException>(() => reader.Dispose());
        Assert.Throws<InvalidOperationException>(() => reader.IntoInner());
        if (!fault) cancellation.Cancel();
        Assert.False(pending.IsCompleted); // This source deliberately ignores cancellation until completed.
        Assert.Equal(0, source.Consumed);
        source.Finish(fault);
        if (fault) Assert.Same(source.Error, await Assert.ThrowsAsync<IOException>(async () => await pending));
        else await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await pending);
        Assert.Equal(1, source.Consumed);
        Assert.Throws<InvalidOperationException>(() => reader.ReadBatchLeaseAsync());
        reader.Dispose();
        Assert.Equal(11, first.GetPayload(0)[0]);
    }

    [Fact]
    public async Task InlineCompletionRequiresConsumptionAndPreservesModeAndEof()
    {
        using var source = new MemoryStream(Recording());
        using var reader = new McapAsyncReader(source, leaveOpen: true);
        var pending = reader.ReadBatchLeaseAsync(1);
        Assert.True(pending.IsCompletedSuccessfully);
        Assert.Throws<InvalidOperationException>(() => reader.Dispose());
        Assert.Throws<InvalidOperationException>(() => reader.ReadBatchLeaseAsync());
        using var first = await pending;
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[64]));
        using var second = await reader.ReadBatchLeaseAsync(1);
        Assert.Equal(22, second!.GetPayload(0)[0]);
        Assert.Null(await reader.ReadBatchLeaseAsync(1));
        Assert.Null(await reader.ReadBatchLeaseAsync(1));
        reader.Dispose();
        Assert.Equal(11, first!.GetPayload(0)[0]);
        using var otherSource = new MemoryStream(Recording());
        using var other = new McapAsyncReader(otherSource);
        await other.ReadNextRecordAsync(new byte[128]);
        Assert.Throws<InvalidOperationException>(() => other.ReadBatchLeaseAsync());
    }

    sealed class PausingStream(byte[] data) : Stream, IValueTaskSource<int>
    {
        readonly MemoryStream inner = new(data, false);
        ManualResetValueTaskSourceCore<int> completion;
        Memory<byte> pending;
        public bool Pause;
        public int Consumed;
        public readonly IOException Error = new("injected asynchronous failure");
        public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken token = default)
        {
            if (!Pause) return new(inner.Read(buffer.Span));
            completion.Reset(); pending = buffer;
            return new(this, completion.Version);
        }
        public void Finish(bool fault)
        {
            if (fault) completion.SetException(Error);
            else { int count = inner.Read(pending.Span); completion.SetResult(count); }
        }
        public int GetResult(short token) { Consumed++; pending = default; return completion.GetResult(token); }
        public ValueTaskSourceStatus GetStatus(short token) => completion.GetStatus(token);
        public void OnCompleted(Action<object?> continuation, object? state, short token, ValueTaskSourceOnCompletedFlags flags)
            => completion.OnCompleted(continuation, state, token, flags);
        public override bool CanRead => true;
        public override bool CanWrite => false;
        public override bool CanSeek => false;
        public override long Length => inner.Length;
        public override long Position { get => inner.Position; set => throw new NotSupportedException(); }
        public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Flush() { }
        protected override void Dispose(bool disposing) { if (disposing) inner.Dispose(); base.Dispose(disposing); }
    }
}
