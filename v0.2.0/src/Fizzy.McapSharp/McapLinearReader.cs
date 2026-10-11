namespace Fizzy.McapSharp;

/// <summary>Top-level records; chunk bodies remain encoded. Does not validate declarations hidden inside chunks.</summary>
public sealed class McapLinearReader : IDisposable
{
    readonly IRecordSource core;
    internal McapLinearReader(IRecordSource core) => this.core = core;
    /// <summary>Copies the supplied input into independent storage; subsequent advancement parses records.</summary>
    public McapLinearReader(ReadOnlySpan<byte> data, McapReaderOptions? options = null)
        => core = new ReadCursorCore(data, CursorMode.TopLevelRecords, options);
    /// <summary>Maps input without copying. Keep the file unchanged until this reader and all storage leases have been disposed.</summary>
    public static McapLinearReader OpenMapped(string path, McapReaderOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        return new(new ReadCursorCore([], CursorMode.TopLevelRecords, options, Path.GetFullPath(path)));
    }
    /// <summary>Copies a raw record body into caller storage. BufferTooSmall retains the item and reports required capacity; parse failures terminate reading.</summary>
    public McapReadStatus ReadNextRecord(Span<byte> destination, out byte opcode, out ulong length)
        => core.ReadNextRecord(destination, out opcode, out length);
    /// <summary>Advances this reader and returns independent mutable copies of record bodies.</summary>
    public IEnumerable<McapRawRecord> ReadRecords() => core.ReadRecords();
    /// <summary>Releases parser state before input. Independent returned records remain valid; repeated disposal is a no-op.</summary>
    public void Dispose() => core.Dispose();
    /// <summary>Releases native state and transfers the underlying Stream. Copied, mapped and path input have no managed Stream to transfer.</summary>
    public Stream IntoInner() => ((IReadProgress)core).IntoInner();
}
