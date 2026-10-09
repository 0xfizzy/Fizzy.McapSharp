using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public sealed partial class McapIndexSnapshot
{
    /// <summary>Opens an independent lazy message cursor that retains storage after the snapshot and prepared descriptor are disposed. The descriptor must identify a valid chunk.</summary>
    public McapBufferReader OpenChunkReader(McapPreparedChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (index.Gate)
        {
            Check(); index.Check();
            int status = Native.fm_snapshot_prepared_chunk_reader(handle, index.Handle, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();
            return new(h, supportsMessages: true);
        }
    }
    /// <summary>Copies the indexed message payload into caller storage. BufferTooSmall reports the required byte length without partial delivery; retries retain pending storage.</summary>
    public McapReadStatus SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry, Span<byte> destination, out McapMessageHeader header, out ulong length) => CallPrepared(Protocol.SnapshotOperation.SeekMessage, index, entry, destination, out header, out length);
    /// <summary>Copies packed little-endian entries: channel ID (u16), log time (u64), chunk-relative offset (u64). Empty channel groups have no rows; BufferTooSmall reports the required byte length.</summary>
    public McapReadStatus ReadMessageIndexes(McapPreparedChunkIndex index, Span<byte> destination, out ulong length) => CallPrepared(Protocol.SnapshotOperation.MessageIndexes, index, default, destination, out _, out length);
    /// <summary>Calculates the chunk-data byte offset from the MCAP origin (the initial Stream position for Stream inputs) from the descriptor and its UTF-8 compression-name length. Does not validate source bytes.</summary>
    public ulong GetCompressedDataOffset(McapPreparedChunkIndex index) { CallPrepared(Protocol.SnapshotOperation.CompressedDataOffset, index, default, [], out _, out var n); return n; }
    /// <summary>Returns independent owned message-index groups for every requested channel, including valid empty groups. Requires a summary and valid message-index offsets.</summary>
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapPreparedChunkIndex index)
    {
        lock (gate)
        {
            ReadMessageIndexes(index, [], out var n); var b = new byte[checked((int)n)]; ReadMessageIndexes(index, b, out _);
            return DecodeMessageIndexes(b, index.ChannelIds);
        }
    }
    /// <summary>Returns an independent owned message from its chunk-relative index entry. Requires a summary and a valid complete chunk descriptor.</summary>
    public McapMessage SeekMessage(McapPreparedChunkIndex index, McapMessageIndexEntry entry) => SeekOwned(null, index, entry);
    /// <summary>Enumerates independent owned messages from one chunk using a child cursor. Disposing the enumeration releases that cursor.</summary>
    public IEnumerable<McapMessage> ReadChunkMessages(McapPreparedChunkIndex index)
    {
        using var reader = OpenChunkReader(index);
        foreach (var message in reader.ReadMessages()) yield return message;
    }
    unsafe McapReadStatus CallPrepared(uint op, McapPreparedChunkIndex index, McapMessageIndexEntry entry, Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (index.Gate)
        {
            Check(); index.Check();
            fixed (byte* p = destination)
            {
                int status = Native.fm_snapshot_prepared_call(handle, op, index.Handle, entry.LogTime, entry.Offset, p, (nuint)destination.Length, out var h, out var r);
                if (status < Protocol.Status.Success) throw Native.ConsumeError(r);
                header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                return status == Protocol.Status.End ? McapReadStatus.EndOfStream : status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    unsafe McapMessage SeekOwned(McapChunkIndex? index, McapPreparedChunkIndex? prepared, McapMessageIndexEntry entry)
    {
        if (prepared is null) ArgumentNullException.ThrowIfNull(index);
        lock (gate) lock (prepared?.Gate ?? gate)
        {
            Check(); prepared?.Check();
            using var sink = new OwnedReadSink(OwnedReadSink.Kind.Message);
            int size = prepared is null ? IndexEncoding.Size(index) : 0;
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            bool added = false;
            try
            {
                if (prepared is null) new IndexEncoding(encoded).Write(index);
                // The second native handle is optional, so hold its SafeHandle explicitly.
                prepared?.Handle.DangerousAddRef(ref added);
                fixed (byte* p = encoded)
                {
                    using var lease = sink.Acquire();
                    int status = Native.fm_snapshot_message_owned(handle, p, (nuint)size, prepared?.Handle.DangerousGetHandle() ?? IntPtr.Zero, entry.LogTime, entry.Offset, sink.Sink, out var r);
                    if (status < Protocol.Status.Success) { var error = Native.ConsumeError(r); sink.ThrowIfError(); throw error; }
                }
                var h = sink.Header;
                return new(GetChannel(h.ChannelId), h.LogTime, h.PublishTime, h.Sequence, (byte[])sink.Value!);
            }
            finally { if (added) prepared!.Handle.DangerousRelease(); NativeMemory.Free(allocated); }
        }
    }
}
