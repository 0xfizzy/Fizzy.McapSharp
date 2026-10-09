using System.Runtime.InteropServices;
using Xunit;

namespace Fizzy.McapSharp.Tests;

public class DeliveryOptimizationTests
{
    internal static byte[] Recording(McapCompression compression, int channels = 1)
    {
        using var storage = new MemoryStream();
        using (var w = new McapWriter(storage, new() { Compression = compression, ChunkSize = null }, true))
        {
            var schema = w.RegisterSchema("s", "raw", [7]);
            for (int i = 0; i < channels; i++)
            {
                var c = w.RegisterChannel("t" + i, "raw", schema, new Dictionary<string, string> { ["k"] = "v" });
                w.WriteMessage(new(c, (uint)i, (ulong)i, 0), new byte[70000]);
                w.WriteMessage(new(c, (uint)i, (ulong)i, 0), []);
            }
            w.WriteMetadata("m", new Dictionary<string, string> { ["k"] = "v" });
            w.WriteAttachment("a", "raw", 1, 0, [1, 2, 3]);
            w.Complete();
        }
        return storage.ToArray();
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void OwnedDeliveryHasOneCopyAndConsumesPending(McapCompression compression)
    {
        var data = Recording(compression);
        using var direct = McapFileReader.OpenMessages(new MemoryStream(data));
        using var owned = McapFileReader.OpenMessages(new MemoryStream(data), options: new());
        var destination = new byte[70000];
        using var iterator = owned.ReadMessages().GetEnumerator();
        while (iterator.MoveNext())
        {
            Assert.Equal(McapReadStatus.Success, direct.ReadNext(destination, out _, out var n));
            Assert.Equal(destination.AsSpan(0, (int)n).ToArray(), iterator.Current.Data);
        }
        Assert.Equal(McapReadStatus.EndOfStream, direct.ReadNext(destination, out _, out _));



        using var pending = McapFileReader.OpenMessages(new MemoryStream(data));
        Assert.Equal(McapReadStatus.BufferTooSmall, pending.ReadNext([], out _, out _));
        using (var messages = pending.ReadMessages().GetEnumerator())
        {
            Assert.True(messages.MoveNext());
            Assert.Equal(70000, messages.Current.Data.Length);

        }

        Assert.Empty(Assert.Single(pending.ReadMessages()).Data);
        using var isolated = McapFileReader.OpenMessages(new MemoryStream(data));
        using (var messages = isolated.ReadMessages().GetEnumerator())
        {
            Assert.True(messages.MoveNext());
            messages.Current.Channel.Schema!.Data[0] = 99;
            ((Dictionary<string, string>)messages.Current.Channel.Metadata)["k"] = "changed";
            Assert.True(messages.MoveNext());
            Assert.Equal(7, messages.Current.Channel.Schema!.Data[0]);
            Assert.Equal("v", messages.Current.Channel.Metadata["k"]);
        }
        using var buffer = new McapBufferReader(data);
        Assert.Equal(McapReadStatus.BufferTooSmall, buffer.ReadNext([], out _, out _));
        var records = buffer.ReadMessages().ToArray();
        Assert.Equal(2, records.Length);
        records[0].Channel.Schema!.Data[0] = 99;
        ((Dictionary<string, string>)records[0].Channel.Metadata)["k"] = "changed";
        Assert.Equal(7, records[1].Channel.Schema!.Data[0]);
        Assert.Equal("v", records[1].Channel.Metadata["k"]);
        buffer.Dispose();
        Assert.Equal(70000, records[0].Data.Length);
    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void ClassifiedScanDoesNotDeliverUnrelatedPayloads(McapCompression compression)
    {
        var data = Recording(compression);
        using var metadata = McapFileReader.OpenRecords(new MemoryStream(data), options: new());
        using var attachment = McapFileReader.OpenRecords(new MemoryStream(data), options: new());
        Assert.Single(metadata.ReadMetadata());
        Assert.Equal(new byte[] { 1, 2, 3 }, Assert.Single(attachment.ReadAttachments()).Data);


    }

    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public void PreparedIndexIsFrozenAndChildrenAreIndependent(McapCompression compression)
    {
        var data = Recording(compression, 110);
        using var snapshot = new McapIndexSnapshot(data, new() { MaxRandomAccessCacheBytes = 16 * 1024 * 1024 });
        using var other = new McapIndexSnapshot(data);
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        Assert.True(IndexEncoding.Size(chunk) > 1024);
        var entries = snapshot.ReadMessageIndexes(chunk);
        using var prepared = new McapPreparedChunkIndex(chunk);
        Assert.Equal(snapshot.GetCompressedDataOffset(chunk), snapshot.GetCompressedDataOffset(prepared));
        Assert.Equal(entries.SelectMany(x => x.Records), snapshot.ReadMessageIndexes(prepared).SelectMany(x => x.Records));
        var entry = entries[0].Records[0];
        var expected = other.SeekMessage(chunk, entry);
        ((Dictionary<ushort, ulong>)chunk.MessageIndexOffsets).Clear();
        Assert.Equal(expected.Data, snapshot.SeekMessage(prepared, entry).Data);
        Assert.Equal(expected.Data, other.SeekMessage(prepared, entry).Data);
        var output = new byte[70000];
        Assert.Equal(McapReadStatus.Success, snapshot.SeekMessage(prepared, entry, output, out _, out _));
        using var child = snapshot.OpenChunkReader(prepared);
        prepared.Dispose(); snapshot.Dispose();
        Assert.Equal(220, child.ReadMessages().Count());
        Assert.Throws<ObjectDisposedException>(() => other.GetCompressedDataOffset(prepared));
        using var invalid = new McapPreparedChunkIndex(chunk with { ChunkLength = 0 });
        Assert.Throws<McapException>(() => other.SeekMessage(invalid, entry));
    }

    [Fact]
    public void PackedRetryRetainsCapacityWithoutGrowingAndCountsEachDelivery()
    {
        using var snapshot = new McapIndexSnapshot(Recording(McapCompression.Zstd, 2));
        using var index = new McapPreparedChunkIndex(snapshot.GetSummary()!.ChunkIndexes.Single());
        snapshot.ReadMessageIndexes(index, [], out var n);
        var output = new byte[(int)n];
        snapshot.ReadMessageIndexes(index, output, out _);
        var expected = output.ToArray();
        for (int i = 0; i < 5; i++)
        {
            snapshot.ReadMessageIndexes(index, [], out _);
            snapshot.ReadMessageIndexes(index, [], out _);

            Assert.Equal(McapReadStatus.Success, snapshot.ReadMessageIndexes(index, output, out _));
            Assert.Equal(expected, output);

        }
    }

    [Fact]
    public async Task PreparedDisposeRacingCallsIsSafe()
    {
        using var snapshot = new McapIndexSnapshot(Recording(McapCompression.None));
        using var index = new McapPreparedChunkIndex(snapshot.GetSummary()!.ChunkIndexes.Single());
        var task = Task.Run(() => { for (int i = 0; i < 1000; i++) { try { snapshot.GetCompressedDataOffset(index); } catch (ObjectDisposedException) { return; } } });
        index.Dispose();
        await task;
        Assert.Throws<ObjectDisposedException>(() => snapshot.GetCompressedDataOffset(index));
    }

    [Fact]
    public unsafe void ManagedCallbackExceptionIsCapturedAndLeaseDoesNotLeak()
    {
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.Message);
        using (sink.Acquire())
        {
            var descriptor = sink.Sink;
            var callback = Marshal.GetDelegateForFunctionPointer<Native.AcceptOwned>(descriptor.Accept);
            var header = new Native.NativeHeader(); nuint copied = 99;
            Assert.Equal(-1, callback(descriptor.Context, 5, &header, null, (nuint)int.MaxValue + 1, &copied));
            Assert.Equal((nuint)0, copied);
            Assert.Throws<OverflowException>(() => sink.ThrowIfError());
        }
        Assert.Equal(IntPtr.Zero, sink.Sink.Context);
        // The same enumeration context can be rooted for another synchronous call.
        using (sink.Acquire()) Assert.NotEqual(IntPtr.Zero, sink.Sink.Context);
        Assert.Equal(IntPtr.Zero, sink.Sink.Context);
    }

    [Fact]
    public void PendingRecordFilteringAndSnapshotOwnedRetryDoNotAdvanceTwice()
    {
        var data = Recording(McapCompression.Zstd);
        using var records = McapFileReader.OpenRecords(new MemoryStream(data));
        Assert.Equal(McapReadStatus.BufferTooSmall, records.ReadNextRecord([], out _, out _));
        Assert.Single(records.ReadMetadata());
        using var snapshot = new McapIndexSnapshot(data);
        var chunk = snapshot.GetSummary()!.ChunkIndexes.Single();
        using var prepared = new McapPreparedChunkIndex(chunk);
        var entries = snapshot.ReadMessageIndexes(prepared)[0].Records;
        Assert.Equal(McapReadStatus.BufferTooSmall, snapshot.SeekMessage(prepared, entries[0], [], out _, out var n));
        Assert.Equal(70000, snapshot.SeekMessage(prepared, entries[0]).Data.Length);

        Assert.Empty(snapshot.SeekMessage(prepared, entries[1]).Data);
    }

    [Fact]
    public unsafe void CallbackFailureIsTerminalAndDoesNotCrossFfi()
    {
        var bytes = Recording(McapCompression.None);
        fixed (byte* data = bytes)
        {
            int status = Native.fm_buffer_reader_open(5, false, data, (nuint)bytes.Length, out var p, out var r);
            Native.Consume(status, r).Json?.Dispose();
            using var handle = new BufferReaderHandle(p);
            Native.AcceptOwned reject = (IntPtr _, byte op, Native.NativeHeader* h, byte* body, nuint n, nuint* count) => -1;
            status = Native.fm_buffer_reader_owned(handle, false, new() { Accept = Marshal.GetFunctionPointerForDelegate(reject) }, out r);
            Assert.True(status < 0);
            Assert.Contains("callback failed", Native.ConsumeError(r).Message);
            status = Native.fm_buffer_reader_next(handle, null, 0, out _, out r);
            Assert.True(status < 0);
            Assert.Contains("Reader failed", Native.ConsumeError(r).Message);
            GC.KeepAlive(reject);
        }
    }
}
