namespace Fizzy.McapSharp;

/// <summary>Outcome of a caller-buffer read or Sans-I/O event poll. Success describes availability, not a particular MCAP opcode.</summary>
public enum McapReadStatus
{
    /// <summary>A record, message, or protocol event is available; inspect the accompanying opcode or event kind.</summary>
    Success,
    /// <summary>The pending item is retained. Retry with a buffer at least as large as the reported required length.</summary>
    BufferTooSmall,
    /// <summary>No further items are available. This does not by itself establish full-file validation.</summary>
    EndOfStream
}

/// <summary>Result of explicit prefix salvage. IsFullyValidated is true only after a strict complete scan; Error describes the failure that ended salvage, if any.</summary>
public sealed record McapRecoveryResult(ulong RecoveredMessageCount, bool IsFullyValidated, McapException? Error);

/// <summary>Whether sequential record scans expose chunks or expand their contents.</summary>
public enum McapRecordMode
{
    /// <summary>Returns top-level records, including compressed chunk records without expansion.</summary>
    TopLevel,
    /// <summary>Expands chunks into their contained records.</summary>
    ExpandChunks
}
