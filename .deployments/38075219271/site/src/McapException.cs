using System.Text.Json;
namespace Fizzy.McapSharp;
/// <summary>Structured MCAP or binding failure classification. Consult Details for kind-specific native fields; this is not a retry policy.</summary>
public enum McapErrorKind
{
    /// <summary>Binding validation, protocol or native panic failure without a more specific kind.</summary>
    Binding,
    /// <summary>Attachment continuation was requested without an active attachment.</summary>
    AttachmentNotInProgress,
    /// <summary>Attachment payload exceeds the declared length.</summary>
    AttachmentTooLarge,
    /// <summary>Attachment was finished before the declared payload length was written.</summary>
    AttachmentIncomplete,
    /// <summary>Leading or trailing MCAP magic is invalid.</summary>
    BadMagic,
    /// <summary>The expected footer is missing or malformed.</summary>
    BadFooter,
    /// <summary>Attachment checksum does not match.</summary>
    BadAttachmentCrc,
    /// <summary>Chunk checksum does not match.</summary>
    BadChunkCrc,
    /// <summary>Data-section checksum does not match.</summary>
    BadDataCrc,
    /// <summary>Summary checksum does not match.</summary>
    BadSummaryCrc,
    /// <summary>An index points to incompatible or invalid data.</summary>
    BadIndex,
    /// <summary>Attachment body length is inconsistent.</summary>
    BadAttachmentLength,
    /// <summary>Chunk body length is inconsistent.</summary>
    BadChunkLength,
    /// <summary>Schema body length is inconsistent.</summary>
    BadSchemaLength,
    /// <summary>The private-record opcode falls in the reserved standard range.</summary>
    PrivateRecordOpcodeIsReserved,
    /// <summary>The same channel ID has conflicting declarations.</summary>
    ConflictingChannels,
    /// <summary>The same schema ID has conflicting declarations.</summary>
    ConflictingSchemas,
    /// <summary>Record decoding failed.</summary>
    Parse,
    /// <summary>Native input or output failed; managed stream errors may instead preserve their original exception type.</summary>
    Io,
    /// <summary>Schema ID zero was used for a schema declaration.</summary>
    InvalidSchemaId,
    /// <summary>Input ended before the required file bytes arrived.</summary>
    UnexpectedEof,
    /// <summary>Expanded chunk data ended before the required record bytes arrived.</summary>
    UnexpectedEoc,
    /// <summary>A message refers to an undeclared channel.</summary>
    UnknownChannel,
    /// <summary>A channel refers to an undeclared schema.</summary>
    UnknownSchema,
    /// <summary>The named compression algorithm is unsupported.</summary>
    UnsupportedCompression,
    /// <summary>A codec failed to decompress chunk contents.</summary>
    DecompressionError,
    /// <summary>Chunk buffering exceeded the supported length.</summary>
    ChunkBufferTooLarge,
    /// <summary>A record exceeds the configured or supported length.</summary>
    RecordTooLarge,
    /// <summary>A chunk exceeds the supported length.</summary>
    ChunkTooLarge,
    /// <summary>Inserted chunk data does not match the requested chunk start.</summary>
    BadChunkStartOffset,
    /// <summary>No additional channel ID is available.</summary>
    TooManyChannels,
    /// <summary>No additional schema ID is available.</summary>
    TooManySchemas,
    /// <summary>Chunk data was inserted when no chunk was requested.</summary>
    UnexpectedChunkDataInserted,
    /// <summary>A terminally failed writer was used again.</summary>
    AttemptedWriteAfterFailure,
    /// <summary>Bytes follow the trailing file magic.</summary>
    BytesAfterEndMagic,
}
/// <summary>A structured MCAP failure. Reader sessions normally become terminal; writer continuation is limited to CanContinueWriting.</summary>
public sealed class McapException : IOException
{
    /// <summary>Whether the writer could continue immediately after this rejection. Not a live state query.</summary>
    public bool CanContinueWriting { get; private init; }
    /// <summary>Failure classification; Binding for exceptions constructed from only a message.</summary>
    public McapErrorKind Kind { get; }
    /// <summary>Independent kind-specific native error details. ValueKind is Undefined when no structured details are available.</summary>
    public JsonElement Details { get; }
    /// <summary>Creates a binding error with no structured details and no writer continuation permission.</summary>
    public McapException(string message) : base(message) { }
    McapException(string message, McapErrorKind kind, JsonElement details) : base(message) { Kind = kind; Details = details; }
    internal static McapException Decode(string text, bool canContinueWriting = false)
    {
        if (!text.StartsWith('{')) return new(text);
        try
        {
            using var j = JsonDocument.Parse(text);
            var r = j.RootElement;
            return new(r.GetProperty("message").GetString()!, Enum.Parse<McapErrorKind>(r.GetProperty("kind").GetString()!), r.GetProperty("details").Clone()) { CanContinueWriting = canContinueWriting };
        }
        catch (JsonException) { return new(text); }
    }
}
