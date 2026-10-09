namespace Fizzy.McapSharp;

// Private ABI values. Mirrored in native/src/protocol.rs; tests/test_protocol.py verifies parity.
internal static class Protocol
{
    internal static class WriterOperation
    {
        internal const uint Schema = 1;
        internal const uint Channel = 2;
        internal const uint Metadata = 4;
        internal const uint Attachment = 5;
        internal const uint Flush = 6;
        internal const uint Complete = 7;
        internal const uint StartAttachment = 8;
        internal const uint AttachmentBytes = 9;
        internal const uint FinishAttachment = 10;
        internal const uint PrivateRecord = 11;
        internal const uint Summary = 12;
        internal const uint FlushToDisk = 13;
    }
    internal static class SnapshotOperation
    {
        internal const uint SeekMessage = 2;
        internal const uint Metadata = 3;
        internal const uint Attachment = 4;
        internal const uint MessageIndexes = 5;
        internal const uint Footer = 6;
        internal const uint CompressedDataOffset = 8;
    }
    internal static class ReaderKind
    {
        internal const uint Session = 0;
        internal const uint Buffer = 1;
    }
    internal static class SummarySource
    {
        internal const uint Snapshot = 0;
        internal const uint Engine = 1;
        internal const uint Writer = 2;
    }
    internal static class EngineKind
    {
        internal const uint Linear = 0;
        internal const uint Summary = 1;
        internal const uint Indexed = 2;
    }
    internal static class DeclarationKind
    {
        internal const uint Schema = 1;
        internal const uint Channel = 2;
    }
    internal static class IndexedControl
    {
        internal const uint InsertChunk = 0;
        internal const uint SetRecordLengthLimit = 1;
        internal const uint ClearRecordLengthLimit = 2;
    }
    internal static class Status
    {
        internal const int Success = 0;
        internal const int End = 1;
        internal const int BufferTooSmall = 2;
        internal const int Error = -1;
    }
    internal static class CallbackStatus
    {
        internal const int Accepted = 0;
        internal const int Stop = 1;
        internal const int Error = -1;
    }
    internal static class WriterStatus
    {
        internal const int SafeRejection = -2;
    }
    internal static class ReaderOpenStatus
    {
        internal const int BufferedSortRequired = 3;
    }
    internal static class BatchStatus
    {
        internal const int VisitorStopped = 3;
    }
    internal static class EngineEvent
    {
        internal const uint End = 0;
        internal const uint Read = 1;
        internal const uint Seek = 2;
        internal const uint Record = 3;
        internal const uint Message = 4;
        internal const uint ReadChunk = 5;
    }
    internal static class BufferMode
    {
        internal const uint Linear = 0;
        internal const uint SansMagic = 1;
        internal const uint FlattenChunks = 2;
        internal const uint Chunk = 3;
        internal const uint RawMessages = 4;
        internal const uint Messages = 5;
    }
}
