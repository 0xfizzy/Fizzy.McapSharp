using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Selects slice-parser semantics. Every mode supports record delivery; only RawMessages and Messages support message delivery.</summary>
internal enum CursorMode
{
    /// <summary>Reads top-level file records, leaving Chunk bodies compressed.</summary>
    TopLevelRecords,
    /// <summary>Reads expanded records from input without start or end magic.</summary>
    ExpandedRecordsWithoutMagic,
    /// <summary>Reads file records with Chunk contents expanded.</summary>
    ExpandedRecords,
    /// <summary>Reads records from a single encoded Chunk body, without the outer record header.</summary>
    ChunkRecords,
    /// <summary>Reads only messages and collects encountered declarations without requiring each message's channel to be declared. Owned ReadMessages results still require a resolvable channel.</summary>
    RawMessages,
    /// <summary>Reads only messages and validates that their channels are declared.</summary>
    Messages
}

/// <summary>One disposable cursor over copied/mapped input, a summary, or a retained chunk. Adapts official slice-reader semantics. Construction copies input; advancement parses records.
/// Unsupported message delivery is rejected without advancement or terminal failure. Summary cursors support records only;
/// snapshot Chunk cursors support messages as well as records.</summary>
internal sealed partial class ReadCursorCore : IRecordSource, IMessageSource, IReadProgress, IDeclarationSource
{
    readonly BufferReaderHandle handle;
    readonly object gate = new();
    readonly BorrowedReadSink borrowed = new();
    readonly bool supportsMessages;
    readonly bool fullyValidating;
    public McapValidationCapability ValidationCapability { get; }
    bool failed;
    void Check()
    {
        borrowed.CheckReentry();
        ObjectDisposedException.ThrowIf(handle.IsClosed, this);
        if (failed) throw new InvalidOperationException("Reader failed; open a new reader.");
    }
    Exception AdvancementError(Native.Result result)
    {
        failed = true;
        return Native.ConsumeError(result);
    }
    void CheckMessages()
    {
        Check();
        if (!supportsMessages) throw new InvalidOperationException("This cursor supports record delivery only.");
    }
    internal ReadCursorCore(IntPtr p, bool supportsMessages) { handle = new(p); this.supportsMessages = supportsMessages; }
    internal unsafe ReadCursorCore(ReadOnlySpan<byte> data, CursorMode mode = CursorMode.Messages, McapReaderOptions? options = null, string? path = null)
    {
        if (!Enum.IsDefined(mode)) throw new ArgumentOutOfRangeException(nameof(mode));
        if (mode == CursorMode.ExpandedRecordsWithoutMagic)
        {
            mode = CursorMode.ExpandedRecords;
            options = (options ?? new()) with { SkipStartMagic = true, SkipEndMagic = true };
        }
        Native.EnsureAvailable();
        if (options?.EmitChunks == true && mode != CursorMode.TopLevelRecords)
            throw new ArgumentException("Select a linear reader to expose chunks.", nameof(options));
        supportsMessages = mode is CursorMode.RawMessages or CursorMode.Messages;
        fullyValidating = options?.IsStrict == true && mode is CursorMode.Messages or CursorMode.ExpandedRecords;
        ValidationCapability = mode is CursorMode.Messages or CursorMode.ExpandedRecords
            ? fullyValidating ? McapValidationCapability.StrictFullScan : McapValidationCapability.FullScan
            : McapValidationCapability.PartialInput;
        var config = Native.Request(new { path, mode = (uint)mode, options = options ?? new() });
        fixed (byte* p = data)
        {
            int status = Native.fm_buffer_reader_configured(config, (nuint)config.Length, p, (nuint)data.Length, out var h, out var r);
            Native.Consume(status, r).Json?.Dispose();
            handle = new(h);
        }
    }
    /// <summary>Maps file contents without copying. Keep the file unchanged until this reader and every lease retaining the mapping are disposed.</summary>
    public static ReadCursorCore OpenMapped(string path, CursorMode mode = CursorMode.Messages, McapReaderOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        return new([], mode, options, Path.GetFullPath(path));
    }
    /// <summary>Copies one raw record body into caller storage. BufferTooSmall retains the record and reports required capacity. The returned opcode identifies the body; length excludes the record header.</summary>
    public unsafe McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
    {
        lock (gate)
        {
            Check();
            fixed (byte* p = destination)
            {
                var status = Native.fm_buffer_reader_next(handle, p, (nuint)destination.Length, out opcode, out var r);
                if (status < Protocol.Status.Success) throw AdvancementError(r);
                length = r.Value;
                return status == Protocol.Status.End ? McapReadStatus.EndOfStream : status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    /// <summary>Copies one message payload into caller storage. BufferTooSmall reports the required byte length and preserves the pending message for retry; EOF is not proof of complete validation.</summary>
    public unsafe McapReadStatus ReadNext(Span<byte> destination, out McapMessageHeader header, out ulong length)
    {
        lock (gate)
        {
            CheckMessages();
            fixed (byte* p = destination)
            {
                int status = Native.fm_buffer_reader_message(handle, p, (nuint)destination.Length, out var h, out var r);
                if (status < Protocol.Status.Success) throw AdvancementError(r);
                header = new(h.ChannelId, h.Sequence, h.LogTime, h.PublishTime); length = r.Value;
                return status == Protocol.Status.End ? McapReadStatus.EndOfStream : status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
            }
        }
    }
    /// <summary>Returns an independent copy of an encountered or snapshot-provided channel and schema. Unknown IDs fail lookup without advancing or terminating the cursor.</summary>
    public McapChannel GetChannel(ushort id)
    {
        lock (gate)
        {
            Check();
            var status = Native.fm_buffer_reader_channel(handle, id, out var r);
            return DeclarationDecoder.Channel(Native.Consume(status, r));
        }
    }
    public McapReadStatus ReadNextMessageRecord(Span<byte> destination, out ulong length)
    {
        lock (gate) { CheckMessages(); return ReadNextRecord(destination, out _, out length); }
    }
    bool ReadOwned(OwnedReadSink sink)
    {
        lock (gate)
        {
            Check();
            sink.Reset();
            using var lease = sink.Acquire();
            int status = Native.fm_buffer_reader_owned(handle, false, sink.Sink, out var r);
            if (status < Protocol.Status.Success) { var error = AdvancementError(r); sink.ThrowIfError(); throw error; }
            return status != Protocol.Status.End;
        }
    }
    /// <summary>Advances this session and yields independently owned copies of raw record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords()
    {
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.Record);
        while (ReadOwned(sink)) yield return (McapRawRecord)sink.Value!;
    }
    /// <summary>Consumes a message-capable cursor and returns independent mutable message and declaration copies. Record-only modes reject enumeration before advancing. RawMessages still requires channel declarations for these resolved results: a missing channel throws after consuming that message, without terminating the cursor. Use header/payload or raw-record delivery for undeclared channels.</summary>
    public IEnumerable<McapMessage> ReadMessages()
    {
        lock (gate) CheckMessages();
        var channels = new Dictionary<ushort, McapChannel>();
        using var sink = new OwnedReadSink(OwnedReadSink.Kind.MessageBody);
        while (ReadOwned(sink))
        {
            if (sink.Value is not byte[] data) continue;
            var h = sink.Header;
            if (!channels.TryGetValue(h.ChannelId, out var channel))
                channels.Add(h.ChannelId, channel = GetChannel(h.ChannelId));
            yield return new(OwnedReadSink.CopyChannel(channel), h.Sequence, h.LogTime, h.PublishTime, data);
        }
    }
    /// <summary>Releases parser state and its retained input; independently retained leases remain valid. Repeated disposal is a no-op.</summary>
    public void Dispose()
    {
        lock (gate)
        {
            borrowed.CheckReentry();
            if (handle.IsClosed) return;
            finalProgress = Progress();
            handle.Dispose();
        }
    }
}
internal sealed class BufferReaderHandle : OwnedNativeHandle
{
    internal BufferReaderHandle(IntPtr p) : base(p) { }
    protected override int ReleaseNative(IntPtr value, out Native.Result result) => Native.fm_buffer_reader_release(value, out result);
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_configured(byte[] config, nuint configLength, byte* p, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_mapped(byte[] config, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_open(uint mode, [MarshalAs(UnmanagedType.I1)] bool ignoreEnd, byte* p, nuint n, out IntPtr h, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_next(BufferReaderHandle h, byte* p, nuint n, out byte opcode, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern unsafe int fm_buffer_reader_message(BufferReaderHandle h, byte* p, nuint n, out NativeHeader header, out Result r);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_buffer_reader_channel(BufferReaderHandle h, ushort id, out Result r);
}
