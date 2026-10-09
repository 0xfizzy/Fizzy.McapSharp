using Xunit;

namespace Fizzy.McapSharp.Tests;

public sealed class WriterCompletionTests
{
    static string TempPath() => Path.Combine(Path.GetTempPath(), $"mcap-completion-{Guid.NewGuid():N}.mcap");

    static byte[] ReadShared(string path)
    {
        using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
        using var bytes = new MemoryStream();
        file.CopyTo(bytes);
        return bytes.ToArray();
    }

    static void Write(McapWriter writer)
    {
        var channel = writer.RegisterChannel("topic", "raw");
        writer.WriteMessage(new(channel, 0, 1, 1), [1, 2, 3]);
    }

    [Theory]
    [InlineData(McapCompression.None)]
    [InlineData(McapCompression.Lz4)]
    [InlineData(McapCompression.Zstd)]
    public void PathCompletionAndPersistenceAreSeparate(McapCompression compression)
    {
        var path = TempPath();
        try
        {
            using var writer = new McapWriter(path, new() { Compression = compression });
            Assert.Throws<InvalidOperationException>(writer.FlushToDisk);
            Write(writer);
            writer.Complete();
            var before = ReadShared(path);
            using (var reader = McapFileReader.OpenMessages(new MemoryStream(before), options: McapReaderOptions.Strict))
                Assert.True(reader.ValidateRemaining() > 0);
            writer.FlushToDisk();
            writer.FlushToDisk();
            writer.Complete();
            Assert.Equal(before, ReadShared(path));
            Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
            writer.Dispose();
            Assert.Throws<ObjectDisposedException>(writer.FlushToDisk);
            using var exclusive = new FileStream(path, FileMode.Open, FileAccess.ReadWrite, FileShare.None);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(McapCompression.None, false)]
    [InlineData(McapCompression.Lz4, true)]
    [InlineData(McapCompression.Zstd, false)]
    public void FileStreamUsesExplicitDurableFlush(McapCompression compression, bool leaveOpen)
    {
        var path = TempPath();
        try
        {
            using var stream = new ObservedFileStream(path);
            using var writer = new McapWriter(stream, new() { Compression = compression }, leaveOpen);
            Write(writer);
            writer.Complete();
            Assert.True(stream.OrdinaryFlushes > 0);
            Assert.Equal(0, stream.DurableFlushes);
            var before = ReadShared(path);
            writer.FlushToDisk();
            writer.FlushToDisk();
            Assert.Equal(2, stream.DurableFlushes);
            Assert.Equal(before, ReadShared(path));
            writer.Dispose();
            Assert.Equal(leaveOpen, stream.CanWrite);
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void UnsupportedStreamsRemainUsableAfterRejection()
    {
        using var stream = new MemoryStream();
        using var writer = new McapWriter(stream, leaveOpen: true);
        Write(writer);
        writer.Complete();
        Assert.Throws<NotSupportedException>(writer.FlushToDisk);
        writer.Complete();
        Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
        Assert.Same(stream, writer.IntoInner());
        Assert.True(stream.CanWrite);
    }

    [Fact]
    public void WrappedFileStreamIsNotUnwrapped()
    {
        var path = TempPath();
        try
        {
            using var file = new ObservedFileStream(path);
            using var stream = new BufferedStream(file);
            using var writer = new McapWriter(stream, leaveOpen: true);
            Write(writer);
            writer.Complete();
            Assert.Throws<NotSupportedException>(writer.FlushToDisk);
            Assert.Equal(0, file.DurableFlushes);
            Assert.Equal(1ul, writer.GetSummary().Statistics!.MessageCount);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void FlushFailuresAreTerminalAndPreserveOriginalException(bool durable)
    {
        var path = TempPath();
        try
        {
            using var stream = new ObservedFileStream(path);
            using var writer = new McapWriter(stream, leaveOpen: true);
            Write(writer);
            McapBufferReader? summary = null;
            if (durable) { writer.Complete(); summary = writer.OpenSummaryRecords(); }
            using var cursor = summary;
            var error = new IOException("injected flush failure");
            if (durable) stream.DurableAction = () => throw error;
            else stream.OrdinaryAction = () => throw error;
            Assert.Same(error, Assert.Throws<IOException>(durable ? writer.FlushToDisk : writer.Complete));
            Assert.Throws<InvalidOperationException>(writer.Complete);
            Assert.Throws<InvalidOperationException>(writer.FlushToDisk);
            Assert.Throws<InvalidOperationException>(() => writer.GetSummary());
            Assert.Throws<InvalidOperationException>(() => writer.OpenSummaryRecords());
            Assert.Throws<InvalidOperationException>(writer.Flush);
            if (cursor is not null) Assert.NotEmpty(cursor.ReadRecords());
            Assert.Same(stream, writer.IntoInner());
            Assert.True(stream.CanWrite);
            stream.DurableAction = stream.OrdinaryAction = null;
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void DurableFlushRejectsReentryAndDoesNotRepeatOnDispose()
    {
        var path = TempPath();
        try
        {
            using var stream = new ObservedFileStream(path);
            using var writer = new McapWriter(stream, leaveOpen: true);
            Write(writer);
            writer.Complete();
            stream.DurableAction = () =>
            {
                Assert.Throws<InvalidOperationException>(writer.FlushToDisk);
                Assert.Throws<InvalidOperationException>(writer.Complete);
                Assert.Throws<InvalidOperationException>(writer.Dispose);
                Assert.Throws<InvalidOperationException>(() => writer.IntoInner());
            };
            writer.FlushToDisk();
            writer.Dispose();
            Assert.Equal(1, stream.DurableFlushes);
        }
        finally { File.Delete(path); }
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void UnfinishedReleaseDoesNotCompleteOrPersist(bool transfer)
    {
        var path = TempPath();
        try
        {
            using var stream = new ObservedFileStream(path);
            using var writer = new McapWriter(stream, leaveOpen: true);
            Write(writer);
            if (transfer) Assert.Same(stream, writer.IntoInner()); else writer.Dispose();
            Assert.Equal(0, stream.DurableFlushes);
            Assert.ThrowsAny<IOException>(() => new McapFileReader(path).Validate());
        }
        finally { File.Delete(path); }
    }

    sealed class ObservedFileStream(string path) : FileStream(path, FileMode.CreateNew, FileAccess.ReadWrite, FileShare.ReadWrite)
    {
        public int OrdinaryFlushes, DurableFlushes;
        public Action? OrdinaryAction, DurableAction;
        public override void Flush() { OrdinaryFlushes++; OrdinaryAction?.Invoke(); base.Flush(false); }
        public override void Flush(bool flushToDisk)
        {
            if (flushToDisk) { DurableFlushes++; DurableAction?.Invoke(); }
            else { OrdinaryFlushes++; OrdinaryAction?.Invoke(); }
            base.Flush(flushToDisk);
        }
    }
}
