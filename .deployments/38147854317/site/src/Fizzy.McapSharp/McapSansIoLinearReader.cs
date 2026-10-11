namespace Fizzy.McapSharp;

/// <summary>Events produced by the linear protocol.</summary>
public enum McapLinearEventKind
{
    /// <summary>The selected protocol completed; this is not proof of full-file validation.</summary>
    End,
    /// <summary>Supply up to Length bytes; empty input signals EOF.</summary>
    Read,
    /// <summary>The next raw record body is available; Opcode identifies it.</summary>
    Record,
}
/// <summary>A linear protocol event. Length is in bytes; Opcode applies only to Record events.</summary>
public readonly record struct McapLinearReadEvent(McapLinearEventKind Kind, byte Opcode, ulong Length);

/// <summary>Caller-driven linear parser. Performs no I/O. Supply requested input before advancing again; buffer-size negotiation preserves pending delivery and other protocol failures are terminal.</summary>
public sealed class McapSansIoLinearReader : IDisposable
{
    readonly SansIoReaderCore core;
    /// <summary>Creates an independent linear parser. Input requests are serviced by the caller.</summary>
    public McapSansIoLinearReader(McapReaderOptions? options = null) => core = SansIoReaderCore.CreateLinear(options);
    /// <summary>Polls the next request or copies delivered bytes. BufferTooSmall reports required Length and retains the same delivery.</summary>
    public McapReadStatus NextEvent(Span<byte> destination, out McapLinearReadEvent readEvent)
    {
        var status = core.NextEvent(destination, out var e);
        var kind = e.Kind switch
        {
            McapReadEventKind.End => McapLinearEventKind.End,
            McapReadEventKind.Read => McapLinearEventKind.Read,
            McapReadEventKind.Record => McapLinearEventKind.Record,
            _ => throw new InvalidOperationException("Unexpected event for this protocol.")
        };
        readEvent = new(kind, e.Opcode, e.Length);
        return status;
    }
    /// <summary>Copies input synchronously for the pending Read request. Empty input signals EOF.</summary>
    public void SupplyInput(ReadOnlySpan<byte> data) => core.SupplyInput(data);
    /// <summary>Releases parser state and reports cleanup failures once.</summary>
    public void Dispose() => core.Dispose();
}
