namespace Fizzy.McapSharp;

public sealed record McapHeader(string Profile, string Library);
public sealed record McapSchemaHeader(ushort Id, string Name, string Encoding);
public sealed record McapChannelRecord(ushort Id, ushort SchemaId, string Topic, string MessageEncoding, IReadOnlyDictionary<string, string> Metadata);
public sealed record McapMessageRecord(McapMessageHeader Header, byte[] Data);
public sealed record McapChunkHeader(ulong MessageStartTime, ulong MessageEndTime, ulong UncompressedSize, uint UncompressedCrc, string Compression, ulong CompressedSize);
public sealed record McapChunkRecord(McapChunkHeader Header, byte[] Data);
public sealed record McapAttachmentHeader(ulong LogTime, ulong CreateTime, string Name, string MediaType);
public sealed record McapAttachmentRecord(McapAttachmentHeader Header, byte[] Data, uint Crc);

public static class McapRecords
{
    public static unsafe McapFooter ReadFooter(ReadOnlySpan<byte> file)
    {
        Native.EnsureAvailable();
        Span<byte> result = stackalloc byte[20];
        fixed (byte* p = file) fixed (byte* dest = result) { int status = Native.fm_footer(p, (nuint)file.Length, dest, out var r); if (status < 0) throw Native.ConsumeError(r); }
        return McapRecordView.Parse(2, result).Footer;
    }
    public static unsafe ulong GetCompressedDataOffset(ulong chunkStartOffset, ReadOnlySpan<byte> compressionUtf8)
    {
        Native.EnsureAvailable();
        fixed (byte* p = compressionUtf8) { int status = Native.fm_chunk_offset(chunkStartOffset, p, (nuint)compressionUtf8.Length, out var r); if (status < 0) throw Native.ConsumeError(r); return r.Value; }
    }

    /// <summary>Uses upstream parse_record validation and returns the corresponding owned field model.</summary>
    public static object Parse(byte opcode, ReadOnlySpan<byte> body)
    {
        var view = McapRecordView.Parse(opcode, body);
        var r = view.Fields;
        switch ((McapOpcode)opcode)
        {
            case McapOpcode.Header: return new McapHeader(r.ReadString(), r.ReadString());
            case McapOpcode.Footer: return view.Footer;
            case McapOpcode.Schema: return new McapSchema(r.ReadUInt16(), r.ReadString(), r.ReadString(), r.ReadBytes(checked((int)r.ReadUInt32())).ToArray());
            case McapOpcode.Channel: return new McapChannelRecord(r.ReadUInt16(), r.ReadUInt16(), r.ReadString(), r.ReadString(), Strings(ref r));
            case McapOpcode.Message: return new McapMessageRecord(view.MessageHeader, view.MessageData.ToArray());
            case McapOpcode.Chunk:
                var chunk = new McapChunkHeader(r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt32(), r.ReadString(), r.ReadUInt64());
                return new McapChunkRecord(chunk, r.ReadBytes(checked((int)chunk.CompressedSize)).ToArray());
            case McapOpcode.MessageIndex:
                var channel = r.ReadUInt16(); var entries = r.ReadCollection(); var messages = new List<McapMessageIndexEntry>();
                while (!entries.IsEmpty) messages.Add(new(entries.ReadUInt64(), entries.ReadUInt64()));
                return new McapMessageIndex(channel, messages);
            case McapOpcode.ChunkIndex:
                var start = r.ReadUInt64(); var end = r.ReadUInt64(); var offset = r.ReadUInt64(); var length = r.ReadUInt64(); var offsets = Integers(ref r);
                return new McapChunkIndex(start, end, offset, length, offsets, r.ReadUInt64(), r.ReadString(), r.ReadUInt64(), r.ReadUInt64());
            case McapOpcode.Attachment:
                var attachment = new McapAttachmentHeader(r.ReadUInt64(), r.ReadUInt64(), r.ReadString(), r.ReadString());
                var data = r.ReadBytes(checked((int)r.ReadUInt64())).ToArray();
                return new McapAttachmentRecord(attachment, data, r.ReadUInt32());
            case McapOpcode.AttachmentIndex: return new McapAttachmentIndex(r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt64(), r.ReadString(), r.ReadString());
            case McapOpcode.Statistics: return new McapStatistics(r.ReadUInt64(), r.ReadUInt16(), r.ReadUInt32(), r.ReadUInt32(), r.ReadUInt32(), r.ReadUInt32(), r.ReadUInt64(), r.ReadUInt64(), Integers(ref r));
            case McapOpcode.Metadata: return new McapMetadata(r.ReadString(), Strings(ref r));
            case McapOpcode.MetadataIndex: return new McapMetadataIndex(r.ReadUInt64(), r.ReadUInt64(), r.ReadString());
            case McapOpcode.SummaryOffset: return view.SummaryOffset;
            case McapOpcode.DataEnd: return view.DataEnd;
            default: return view.ToOwned();
        }
    }
    static Dictionary<string, string> Strings(ref McapRecordFields fields)
    {
        var r = fields.ReadCollection(); var result = new Dictionary<string, string>();
        while (!r.IsEmpty) result[r.ReadString()] = r.ReadString();
        return result;
    }
    static Dictionary<ushort, ulong> Integers(ref McapRecordFields fields)
    {
        var r = fields.ReadCollection(); var result = new Dictionary<ushort, ulong>();
        while (!r.IsEmpty) result[r.ReadUInt16()] = r.ReadUInt64();
        return result;
    }
}
