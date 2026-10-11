using System.Buffers.Binary;

namespace Fizzy.McapSharp;
internal ref struct RecordDecoder
{
    McapRecordFields fields;
    internal RecordDecoder(ReadOnlySpan<byte> data) => fields = new(data);
    internal ushort U16() => fields.ReadUInt16();
    internal uint U32() => fields.ReadUInt32();
    internal ulong U64() => fields.ReadUInt64();
    internal string Text() => fields.ReadString();
    internal byte[] Bytes(int n) => fields.ReadBytes(n).ToArray();
    internal Dictionary<string, string> Map() => McapRecords.Strings(ref fields);

    internal static McapSchema Schema(ReadOnlySpan<byte> data)
    {
        var r = new RecordDecoder(data);
        return new(r.U16(), r.Text(), r.Text(), r.Bytes(checked((int)r.U32())));
    }

    internal static McapMetadata Metadata(ReadOnlySpan<byte> data)
    {
        var r = new RecordDecoder(data);
        return new(r.Text(), r.Map());
    }

    internal static McapAttachment Attachment(ReadOnlySpan<byte> data)
    {
        var r = new RecordDecoder(data);
        var log = r.U64();
        var create = r.U64();
        var name = r.Text();
        var media = r.Text();
        return new(name, media, log, create, r.Bytes(checked((int)r.U64())));
    }

    internal static ushort ChannelId(ReadOnlySpan<byte> data) => BinaryPrimitives.ReadUInt16LittleEndian(data);
    internal static McapMessageIndex MessageIndex(ReadOnlySpan<byte> data)
    {
        var r = new RecordDecoder(data);
        var id = r.U16();
        uint length = r.U32();
        if (length % 16 != 0)
            throw new McapException("Invalid message index length.");
        var entries = new McapMessageIndexEntry[length / 16];
        for (int i = 0; i < entries.Length; i++)
            entries[i] = new(r.U64(), r.U64());
        return new(id, entries);
    }
}
