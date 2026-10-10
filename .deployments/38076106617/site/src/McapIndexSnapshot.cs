using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Owns copied or explicitly mapped input and its official summary, independently of the originating session.
/// Snapshot message callbacks must not invoke operations on any snapshot or prepared chunk index, including disposal.</summary>
public sealed partial class McapIndexSnapshot : IDisposable
{
    readonly SnapshotHandle handle;
    readonly object gate = new();
    readonly BorrowedReadSink borrowed = new();
    // Check the thread-wide callback scope before acquiring any snapshot or descriptor lock.
    static void CheckBeforeLock() => McapPreparedChunkIndex.CheckCallbackReentry();
    void Check() { borrowed.CheckReentry(); ObjectDisposedException.ThrowIf(handle.IsClosed, this); }
    internal McapIndexSnapshot(IntPtr p) => handle = new(p);
    /// <summary>Copies the complete input into independent storage and reads its summary. Cache allowance excludes this input copy; summary success is not full-file validation.</summary>
    public McapIndexSnapshot(ReadOnlySpan<byte> data) : this(data, null) { }
    /// <summary>Copies the complete input into independent storage and reads its summary. Cache allowance excludes this input copy; summary success is not full-file validation.</summary>
    public unsafe McapIndexSnapshot(ReadOnlySpan<byte> data, McapIndexSnapshotOptions? options)
    {
        Native.EnsureAvailable();
        var config = Native.Request(options ?? new());
        fixed (byte* p = data) { var status = Native.fm_snapshot_bytes_options(p, (nuint)data.Length, config, (nuint)config.Length, out var h, out var r); Native.Consume(status, r).Json?.Dispose(); handle = new(h); }
    }
    /// <summary>Maps a file without an owned input copy. Keep the file unchanged until this snapshot, all child cursors and all leases retaining its mapping are disposed.</summary>
    public static McapIndexSnapshot OpenMapped(string path, McapIndexSnapshotOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        Native.EnsureAvailable();
        var config = Native.Request(new { path, options });
        int status = Native.fm_snapshot_mapped(config, (nuint)config.Length, out var h, out var r);
        Native.Consume(status, r).Json?.Dispose();

        return new(h);
    }
    /// <summary>Returns an independent owned summary, or null when absent. Summary availability does not establish full-file validation.</summary>
    public McapSummary? GetSummary()
    {
        CheckBeforeLock();
        lock (gate) { Check(); var status = Native.fm_snapshot_summary(handle, out var r); var response = Native.Consume(status, r); using var j = response.Json; return j?.RootElement.Deserialize<McapSummary>(JsonSupport.Options); }
    }
    /// <summary>Returns independent owned message-index groups for every requested channel, including valid empty groups. Requires a summary and valid message-index offsets.</summary>
    public IReadOnlyList<McapMessageIndex> ReadMessageIndexes(McapChunkIndex chunk)
    {
        CheckBeforeLock();
        lock (gate)
        {
            ReadMessageIndexes(chunk, [], out var n); var b = new byte[checked((int)n)]; ReadMessageIndexes(chunk, b, out _);
            return DecodeMessageIndexes(b, chunk.MessageIndexOffsets.Keys);
        }
    }
    static IReadOnlyList<McapMessageIndex> DecodeMessageIndexes(ReadOnlySpan<byte> b, IEnumerable<ushort> channelIds)
    {
        var fields = new McapRecordFields(b); var groups = new Dictionary<ushort, List<McapMessageIndexEntry>>();
        // Native validation establishes that every requested channel has a real index
        // record. Packed rows alone cannot represent a valid empty index record.
        foreach (var id in channelIds) groups.Add(id, []);
        while (!fields.IsEmpty) { var id = fields.ReadUInt16(); if (!groups.TryGetValue(id, out var list)) groups.Add(id, list = []); list.Add(new(fields.ReadUInt64(), fields.ReadUInt64())); }
        return groups.OrderBy(g => g.Key).Select(g => new McapMessageIndex(g.Key, g.Value)).ToArray();
    }
    /// <summary>Opens an independent summary-record cursor that retains the summary after this snapshot is disposed. Requires an available summary.</summary>
    public McapReadCursor OpenSummaryRecords() { CheckBeforeLock(); lock (gate) { Check(); return Native.SummaryRecords(Protocol.SummarySource.Snapshot, handle); } }
    /// <summary>Returns an independent owned channel declaration from the summary, including schema and metadata.</summary>
    public McapChannel GetChannel(ushort id)
    {
        CheckBeforeLock();
        lock (gate) { Check(); var status = Native.fm_snapshot_channel(handle, id, out var r); return DeclarationDecoder.Channel(Native.Consume(status, r)); }
    }
    /// <summary>Returns an independent owned message from its chunk-relative index entry. Requires a summary and a valid complete chunk descriptor.</summary>
    public McapMessage SeekMessage(McapChunkIndex chunk, McapMessageIndexEntry entry) => SeekOwned(chunk, null, entry);
    /// <summary>Enumerates independent owned messages from one chunk using a child cursor. Disposing the enumeration releases that cursor.</summary>
    public IEnumerable<McapMessage> ReadChunkMessages(McapChunkIndex chunk)
    {
        using var reader = OpenChunkReader(chunk);
        foreach (var message in reader.ReadMessages()) yield return message;
    }
    /// <summary>Opens an independent lazy cursor that remains valid after this snapshot is disposed. Traverse one cursor to reuse chunk parsing across messages; reopening starts a new traversal.</summary>
    public unsafe McapReadCursor OpenChunkReader(McapChunkIndex index)
    {
        ArgumentNullException.ThrowIfNull(index);
        CheckBeforeLock();
        lock (gate)
        {
            Check();
            int size = IndexEncoding.Size(index);
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            try
            {
                new IndexEncoding(encoded).Write(index);
                fixed (byte* body = encoded)
                {
                    int status = Native.fm_snapshot_chunk_reader(handle, body, (nuint)size, out var p, out var r);
                    Native.Consume(status, r).Json?.Dispose();
                    return new(p, supportsMessages: true);
                }
            }
            finally { NativeMemory.Free(allocated); }
        }
    }
    /// <summary>Copies the indexed message payload into caller storage. BufferTooSmall reports the required byte length without partial delivery; retries retain pending storage.</summary>
    public McapReadStatus SeekMessage(McapChunkIndex chunk, McapMessageIndexEntry message, Span<byte> destination, out McapMessageHeader header, out ulong length) => Call(Protocol.SnapshotOperation.SeekMessage, chunk, message, destination, out header, out length);
    /// <summary>Copies the complete indexed record body into caller storage. BufferTooSmall reports the required byte length without a partial copy; no summary is required.</summary>
    public McapReadStatus ReadMetadata(McapMetadataIndex index, Span<byte> destination, out ulong length) => Call(Protocol.SnapshotOperation.Metadata, index, default, destination, out _, out length);
    /// <summary>Copies the complete indexed record body into caller storage. BufferTooSmall reports the required byte length without a partial copy; no summary is required.</summary>
    public McapReadStatus ReadAttachment(McapAttachmentIndex index, Span<byte> destination, out ulong length) => Call(Protocol.SnapshotOperation.Attachment, index, default, destination, out _, out length);
    /// <summary>Copies packed entries: channel ID (u16), log time (u64), chunk-relative offset (u64), all little endian.</summary>
    public McapReadStatus ReadMessageIndexes(McapChunkIndex index, Span<byte> destination, out ulong length) => Call(Protocol.SnapshotOperation.MessageIndexes, index, default, destination, out _, out length);
    /// <summary>Calculates the chunk-data byte offset from the MCAP origin (the initial Stream position for Stream inputs) from the descriptor and its UTF-8 compression-name length. Does not validate source bytes.</summary>
    public ulong GetCompressedDataOffset(McapChunkIndex index) { Call(Protocol.SnapshotOperation.CompressedDataOffset, index, default, [], out _, out var value); return value; }
    /// <summary>Reads the footer from the retained input. Does not validate the complete file.</summary>
    public McapFooter ReadFooter()
    {
        Span<byte> b = stackalloc byte[20];
        Call(Protocol.SnapshotOperation.Footer, null, default, b, out _, out _);
        var v = McapRecordView.Parse(2, b);
        return v.Footer;
    }
    /// <summary>Returns an independent owned record after validating its indexed range. No summary is required.</summary>
    public McapMetadata ReadMetadata(McapMetadataIndex index) => (McapMetadata)ReadOwned(Protocol.SnapshotOperation.Metadata, index, OwnedReadSink.Kind.Metadata);
    /// <summary>Returns an independent owned record after validating its indexed range. Copies the payload directly into its final array; no summary is required.</summary>
    public McapAttachment ReadAttachment(McapAttachmentIndex index) => (McapAttachment)ReadOwned(Protocol.SnapshotOperation.Attachment, index, OwnedReadSink.Kind.Attachment);
    unsafe object ReadOwned(uint op, object index, OwnedReadSink.Kind kind)
    {
        ArgumentNullException.ThrowIfNull(index);
        CheckBeforeLock();
        lock (gate)
        {
            Check();
            using var sink = new OwnedReadSink(kind);
            int size = IndexEncoding.Size(index);
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            try
            {
                new IndexEncoding(encoded).Write(index);
                using var lease = sink.Acquire();
                fixed (byte* body = encoded)
                {
                    int status = Native.fm_snapshot_record_owned(handle, op, body, (nuint)size, sink.Sink, out var result);
                    if (status < Protocol.Status.Success) { var error = Native.ConsumeError(result); sink.ThrowIfError(); throw error; }
                }
                return sink.Value!;
            }
            finally { NativeMemory.Free(allocated); }
        }
    }
    unsafe McapReadStatus Call(uint op, object? index, McapMessageIndexEntry message, Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        CheckBeforeLock();
        lock (gate)
        {
            Check();
            if (op is Protocol.SnapshotOperation.SeekMessage or Protocol.SnapshotOperation.Metadata or Protocol.SnapshotOperation.Attachment or Protocol.SnapshotOperation.MessageIndexes or Protocol.SnapshotOperation.CompressedDataOffset) ArgumentNullException.ThrowIfNull(index);
            int size = IndexEncoding.Size(index);
            byte* allocated = size > 1024 ? (byte*)NativeMemory.Alloc((nuint)size) : null;
            Span<byte> encoded = size <= 1024 ? stackalloc byte[size] : new Span<byte>(allocated, size);
            try
            {
                new IndexEncoding(encoded).Write(index);
                fixed (byte* p = destination) fixed (byte* body = encoded)
                {
                    int status = Native.fm_snapshot_call(handle, op, body, (nuint)encoded.Length, message.LogTime, message.Offset, p, (nuint)destination.Length, out var h, out var r);
                    if (status < Protocol.Status.Success) throw Native.ConsumeError(r);
                    header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                    return status == Protocol.Status.End ? McapReadStatus.EndOfStream : status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
                }
            }
            finally { NativeMemory.Free(allocated); }
        }
    }
    /// <summary>Releases this owner once. Independently retained cursors and leases remain valid; repeated disposal does not replay release.</summary>
    public void Dispose() { CheckBeforeLock(); lock (gate) { borrowed.CheckReentry(); handle.Dispose(); } }
}
