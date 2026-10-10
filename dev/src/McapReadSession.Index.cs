namespace Fizzy.McapSharp;

public sealed partial class McapReadSession
{
    /// <summary>Copies the complete seekable source into an independent snapshot while preserving this cursor position. Source I/O or copy failure terminates the session; successful snapshots outlive it.</summary>
    public McapIndexSnapshot OpenIndexSnapshot() => OpenIndexSnapshot(null);
    /// <summary>Copies the complete seekable source into an independent snapshot while preserving this cursor position. Source I/O or copy failure terminates the session; successful snapshots outlive it.</summary>
    public McapIndexSnapshot OpenIndexSnapshot(McapIndexSnapshotOptions? options)
    {
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Snapshot requires a seekable source.");
            var config = options is null ? Array.Empty<byte>() : Native.Request(options);
            try
            {
                int status = Native.fm_snapshot_open_options(handle, config, (nuint)config.Length, out var p, out var r);
                Native.ConsumeReader(status, r, handle.Bridge).Json?.Dispose();
                return new(p);
            }
            catch { failed = true; throw; }
        }
    }
    /// <summary>Reads and validates the record at a byte offset from the MCAP origin (the initial Stream position for Stream inputs) without advancing this cursor. Copies the body into caller storage; BufferTooSmall reports the required byte length without a partial copy. Native or I/O failure terminates the session.</summary>
    public unsafe McapReadStatus ReadRecordAt(ulong offset, Span<byte> destination, out byte opcode, out ulong length)
    {
        lock (gate)
        {
            Check();
            if (!seekable) throw new NotSupportedException("Random access requires a seekable source.");
            try
            {
                fixed (byte* p = destination)
                {
                    int status = Native.fm_reader_record_into(handle, offset, p, (nuint)destination.Length, out opcode, out var r);
                    if (status < Protocol.Status.Success)
                    {
                        var error = Native.ConsumeError(r);
                        if (handle.Bridge is not null) handle.Bridge.ThrowOperationError(error);
                        throw error;
                    }
                    length = r.Value;
                    return status == Protocol.Status.BufferTooSmall ? McapReadStatus.BufferTooSmall : McapReadStatus.Success;
                }
            }
            catch { failed = true; throw; }
        }
    }
}
