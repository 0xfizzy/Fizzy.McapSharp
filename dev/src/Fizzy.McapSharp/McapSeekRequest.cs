namespace Fizzy.McapSharp;

/// <summary>A requested source position. Begin uses an unsigned absolute position; Current and End use a signed relative offset. Positions are relative to the MCAP origin.</summary>
public readonly struct McapSeekRequest
{
    readonly ulong offset;
    /// <summary>Reference point for the requested seek.</summary>
    public SeekOrigin Origin { get; }
    /// <summary>Absolute MCAP position for Begin; throws InvalidOperationException for relative seeks.</summary>
    public ulong AbsolutePosition => Origin == SeekOrigin.Begin ? offset : throw new InvalidOperationException("This is a relative seek.");
    /// <summary>Signed displacement for Current or End; throws InvalidOperationException for Begin.</summary>
    public long RelativeOffset => Origin != SeekOrigin.Begin ? unchecked((long)offset) : throw new InvalidOperationException("This is an absolute seek.");
    internal McapSeekRequest(SeekOrigin origin, ulong offset) => (Origin, this.offset) = (origin, offset);
}
