using System.Text.Json;
namespace Fizzy.McapSharp;
public enum McapErrorKind { Binding, AttachmentNotInProgress, AttachmentTooLarge, AttachmentIncomplete, BadMagic, BadFooter, BadAttachmentCrc, BadChunkCrc, BadDataCrc, BadSummaryCrc, BadIndex, BadAttachmentLength, BadChunkLength, BadSchemaLength, PrivateRecordOpcodeIsReserved, ConflictingChannels, ConflictingSchemas, Parse, Io, InvalidSchemaId, UnexpectedEof, UnexpectedEoc, UnknownChannel, UnknownSchema, UnsupportedCompression, DecompressionError, ChunkBufferTooLarge, RecordTooLarge, ChunkTooLarge, BadChunkStartOffset, TooManyChannels, TooManySchemas, UnexpectedChunkDataInserted, AttemptedWriteAfterFailure, BytesAfterEndMagic }
public sealed class McapException : IOException
{
    /// <summary>Whether the writer could continue immediately after this rejection. Not a live state query.</summary>
    public bool CanContinueWriting { get; private init; }
    public McapErrorKind Kind { get; }
    public JsonElement Details { get; }
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
