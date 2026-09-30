using System.Buffers.Binary;
using System.Text;

namespace Fizzy.McapSharp;
internal ref struct RecordDecoder
{
    ReadOnlySpan<byte> data;
    internal RecordDecoder(ReadOnlySpan<byte> data) => this.data = data;
    ReadOnlySpan<byte> Take(int n)
    {
        if (n < 0 || n > data.Length)
            throw new McapException("Invalid record length.");
        var v = data[..n];
        data = data[n..];
        return v;
    }

    internal ushort U16() => BinaryPrimitives.ReadUInt16LittleEndian(Take(2));
    internal uint U32() => BinaryPrimitives.ReadUInt32LittleEndian(Take(4));
    internal ulong U64() => BinaryPrimitives.ReadUInt64LittleEndian(Take(8));
    internal string Text() => Encoding.UTF8.GetString(Take(checked((int)U32())));
    internal byte[] Bytes(int n) => Take(n).ToArray();
    internal Dictionary<string, string> Map()
    {
        var d = new RecordDecoder(Take(checked((int)U32())));
        var m = new Dictionary<string, string>();
        while (!d.data.IsEmpty)
            m.Add(d.Text(), d.Text());
        return m;
    }

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
