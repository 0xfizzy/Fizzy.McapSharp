using Fizzy.McapSharp;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public sealed class LifecycleFailureTests
{
    static byte[] Sample()
    {
        using var stream = new MemoryStream();
        using (var writer = new McapWriter(stream, new() { UseChunks = false, Compression = McapCompression.None }, leaveOpen: true))
        {
            var channel = writer.RegisterChannel("test", "raw");
            writer.WriteMessage(new McapMessageHeader(channel, 0, 1, 1), [42]);
            writer.Complete();
        }
        return stream.ToArray();
    }

    sealed class FailingStream : MemoryStream
    {
        internal FailingStream(byte[] data) { Write(data); Position = 0; }
        internal readonly IOException ReadError = new("read failed");
        internal readonly IOException SeekError = new("restore failed");
        internal readonly IOException DisposeError = new("dispose failed");
        internal bool FailRead, FailRestore, FailDispose, FailRestoreAfterRead;
        bool readCompleted;
        bool readFailed;
        internal int DisposeCalls;
        internal Action? OnDispose;
        public override int Read(Span<byte> buffer)
        {
            if (FailRead) { readFailed = true; throw ReadError; }
            int count = base.Read(buffer);
            readCompleted = true;
            return count;
        }
        public override long Seek(long offset, SeekOrigin origin)
        {
            if ((FailRestore && readFailed) || (FailRestoreAfterRead && readCompleted && origin == SeekOrigin.Begin && offset == 0)) throw SeekError;
            return base.Seek(offset, origin);
        }
        protected override void Dispose(bool disposing)
        {
            if (!disposing) return;
            DisposeCalls++;
            OnDispose?.Invoke();
            if (FailDispose) throw DisposeError;
            base.Dispose(disposing);
        }
    }

    [Theory]
    [InlineData("writer")]
    [InlineData("reader")]
    [InlineData("async")]
    public void ExplicitDisposalReportsOwnedStreamFailureOnceAndKeepsExclusion(string kind)
    {
        var stream = new FailingStream(kind == "writer" ? [] : Sample()) { FailDispose = true };
        // Writers start at the current end and append their MCAP section.
        if (kind == "writer") stream.Position = stream.Length;
        IDisposable owner = kind switch
        {
            "writer" => new McapWriter(stream),
            "reader" => McapFileReader.OpenRecords(stream),
            _ => new McapAsyncReader(stream)
        };
        stream.OnDispose = () => Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
        Assert.Same(stream.DisposeError, Assert.Throws<IOException>(owner.Dispose));
        owner.Dispose();
        Assert.Equal(1, stream.DisposeCalls);
        Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
    }

    [Theory]
    [InlineData("summary")]
    [InlineData("record")]
    [InlineData("recordInto")]
    [InlineData("snapshot")]
    public void AuxiliaryIoFailureIsTerminal(string operation)
    {
        using var stream = new FailingStream(Sample());
        using var reader = McapFileReader.OpenRecords(stream, leaveOpen: true);
        stream.FailRead = true;
        Assert.Same(stream.ReadError, Assert.Throws<IOException>(() => Run(reader, operation)));
        stream.FailRead = false;
        Assert.Throws<InvalidOperationException>(() => reader.ReadNextRecord(new byte[1024], out _, out _));
    }

    static void Run(McapReadSession reader, string operation)
    {
        switch (operation)
        {
            case "summary": reader.GetSummary(); break;
            case "record": reader.ReadRecordAt(8); break;
            case "recordInto": reader.ReadRecordAt(8, new byte[1024], out _, out _); break;
            case "snapshot": using (reader.OpenIndexSnapshot()) { } break;
        }
    }

    [Theory]
    [InlineData("summary")]
    [InlineData("record")]
    [InlineData("recordInto")]
    [InlineData("snapshot")]
    public void OperationAndRestoreFailuresBothRemainObservable(string operation)
    {
        using var stream = new FailingStream(Sample());
        using var reader = McapFileReader.OpenRecords(stream, leaveOpen: true);
        stream.FailRead = stream.FailRestore = true;
        var failure = Assert.Throws<AggregateException>(() => Run(reader, operation)).Flatten();
        Assert.Contains(stream.ReadError, failure.InnerExceptions);
        Assert.Contains(stream.SeekError, failure.InnerExceptions);
    }

    [Theory]
    [InlineData("reader")]
    [InlineData("async")]
    public void OwnershipTransferReleasesExclusionWithoutDisposingStream(string kind)
    {
        var stream = new FailingStream(Sample());
        Stream transferred;
        if (kind == "reader")
        {
            using var reader = McapFileReader.OpenRecords(stream);
            transferred = reader.IntoInner();
        }
        else
        {
            using var reader = new McapAsyncReader(stream);
            transferred = reader.IntoInner();
        }
        Assert.Same(stream, transferred);
        Assert.Equal(0, stream.DisposeCalls);
        stream.Position = 0;
        using (var next = new McapAsyncReader(stream, leaveOpen: true)) { }
        stream.Dispose();
    }

    [Fact]
    public void InitializationPreservesOperationAndDisposalFailure()
    {
        var stream = new FailingStream(Sample()) { FailRead = true, FailDispose = true };
        var failure = Assert.Throws<AggregateException>(() => McapFileReader.OpenMessages(stream, new() { Order = McapReadOrder.LogTime })).Flatten();
        Assert.Contains(stream.ReadError, failure.InnerExceptions);
        Assert.Contains(stream.DisposeError, failure.InnerExceptions);
        Assert.Equal(1, stream.DisposeCalls);
        Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
    }
    [Fact]
    public void InitializationPreservesNativeFormatErrorAndRestoreCallbackFailure()
    {
        var malformed = Sample();
        malformed[^1] ^= 0xff; // The summary reader rejects the trailing magic before reading the summary.
        using var stream = new FailingStream(malformed) { FailRestoreAfterRead = true };
        var failure = Assert.Throws<AggregateException>(() =>
            McapFileReader.OpenMessages(stream, new() { Order = McapReadOrder.LogTime }, leaveOpen: true)).Flatten();
        Assert.Contains(stream.SeekError, failure.InnerExceptions);
        var native = Assert.Single(failure.InnerExceptions.OfType<McapException>());
        Assert.Equal("operationRestore", native.Details.GetProperty("code").GetString());
        Assert.Equal("BadMagic", native.Details.GetProperty("operation").GetProperty("kind").GetString());
        Assert.Equal("Binding", native.Details.GetProperty("cleanup").GetProperty("kind").GetString());
        Assert.Equal("Managed Stream callback failed", native.Details.GetProperty("cleanup").GetProperty("message").GetString());
        Assert.Equal(0, stream.DisposeCalls);
        // Failed construction released its native owner, so leaveOpen ownership is safely returned.
        using var next = new McapAsyncReader(stream, leaveOpen: true);
    }

    // These handles isolate the public release contract from the operating system:
    // returning -1 represents a consumed native owner; throwing represents no ABI result.
    sealed class ReleaseProbe(StreamBridge bridge, bool interopThrows = false) : OwnedNativeHandle((IntPtr)1, bridge)
    {
        internal int Calls;
        internal readonly EntryPointNotFoundException InteropError = new("release entry point unavailable");
        protected override int ReleaseNative(IntPtr value, out Native.Result result)
        {
            Calls++;
            result = default;
            if (interopThrows) throw InteropError;
            return -1;
        }
        internal void FinalizeFallback() => Dispose(false);
    }

    [Fact]
    public void ReturnedNativeReleaseFailureIsVisibleAndConsumesOwnerOnce()
    {
        var stream = new FailingStream(Sample());
        var bridge = new StreamBridge(stream, writing: false, leaveOpen: false);
        var handle = new ReleaseProbe(bridge);
        Assert.Throws<McapException>(handle.Dispose);
        Assert.True(handle.NativeReleased);
        Assert.True(handle.IsClosed);
        Assert.Equal(1, stream.DisposeCalls);
        handle.Dispose();
        Assert.Equal(1, handle.Calls);
    }

    [Fact]
    public void SimultaneousNativeAndStreamReleaseFailuresAreBothReportedOnce()
    {
        var stream = new FailingStream(Sample()) { FailDispose = true };
        var handle = new ReleaseProbe(new StreamBridge(stream, writing: false, leaveOpen: false));
        var error = Assert.Throws<AggregateException>(handle.Dispose).Flatten();
        Assert.Contains(error.InnerExceptions, e => e is McapException);
        Assert.Contains(stream.DisposeError, error.InnerExceptions);
        handle.Dispose();
        Assert.Equal(1, handle.Calls);
        Assert.Equal(1, stream.DisposeCalls);
        Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
    }

    [Fact]
    public void InteropFailureRetainsDependencyAndExclusionWithoutRetry()
    {
        var stream = new FailingStream(Sample());
        var bridge = new StreamBridge(stream, writing: false, leaveOpen: false);
        var handle = new ReleaseProbe(bridge, interopThrows: true);
        Assert.Same(handle.InteropError, Assert.Throws<EntryPointNotFoundException>(handle.Dispose));
        Assert.False(handle.NativeReleased);
        Assert.Equal(0, stream.DisposeCalls);
        Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
        handle.Dispose();
        Assert.Equal(1, handle.Calls);
        // This test double owns no native pointer; explicitly discharge its retained bridge.
        bridge.Release();
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void FailedNativeReleaseCannotReportSuccessfulOwnershipTransfer(bool interopThrows)
    {
        var stream = new FailingStream(Sample());
        var bridge = new StreamBridge(stream, writing: false, leaveOpen: false);
        var handle = new ReleaseProbe(bridge, interopThrows);
        if (interopThrows)
        {
            Assert.Same(handle.InteropError, Assert.Throws<EntryPointNotFoundException>(() => handle.Transfer()));
            Assert.Equal(0, stream.DisposeCalls);
            Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
            bridge.Release();
        }
        else
        {
            Assert.Throws<McapException>(() => handle.Transfer());
            Assert.Equal(1, stream.DisposeCalls);
        }
        handle.Dispose();
        Assert.Equal(1, handle.Calls);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void FinalizerFallbackNeverThrowsAndNeverReplaysRelease(bool interopThrows)
    {
        var stream = new FailingStream(Sample()) { FailDispose = true };
        var bridge = new StreamBridge(stream, writing: false, leaveOpen: false);
        var handle = new ReleaseProbe(bridge, interopThrows);
        handle.FinalizeFallback();
        handle.FinalizeFallback();
        Assert.Equal(1, handle.Calls);
        Assert.Equal(interopThrows ? 0 : 1, stream.DisposeCalls);
        Assert.Throws<InvalidOperationException>(() => new McapAsyncReader(stream));
        if (interopThrows)
        {
            stream.FailDispose = false;
            bridge.Release();
        }
    }

}
