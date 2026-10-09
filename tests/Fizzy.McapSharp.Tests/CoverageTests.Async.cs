using Xunit;

namespace Fizzy.McapSharp.Tests;

public partial class CoverageTests
{
    [Fact]
    public async Task SuspendedCancellationRejectsConcurrentOperations()
    {
        using var stream = new PendingStream();
        using var reader = new McapAsyncReader(stream, leaveOpen: true);
        using var cancellation = new CancellationTokenSource();
        var pending = reader.ReadNextRecordAsync(new byte[256], cancellation.Token);
        Assert.False(pending.IsCompleted);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[256]));
        Assert.Throws<InvalidOperationException>(() => reader.Dispose());
        Assert.Throws<InvalidOperationException>(() => McapReaderFactory.OpenMessages(stream));
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await pending);
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecordAsync(new byte[256]));
    }
    sealed class PendingStream : Stream
    {
        readonly TaskCompletionSource<int> source = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken token = default) => new(source.Task.WaitAsync(token));
        public override bool CanRead => true;
        public override bool CanWrite => false;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override void Flush() => throw new NotSupportedException();
        public override int Read(byte[] b, int o, int n) => throw new NotSupportedException();
        public override long Seek(long o, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long n) => throw new NotSupportedException();
        public override void Write(byte[] b, int o, int n) => throw new NotSupportedException();
    }
}
