using System.Buffers.Binary;
using System.Text;

namespace Fizzy.McapSharp;

// Standard MCAP index record bodies, consumed synchronously by the native bridge.
internal ref struct IndexEncoding(Span<byte> destination)
{
    Span<byte> remaining = destination;
    static int StringSize(string value) => checked(4 + Encoding.UTF8.GetByteCount(value));
    internal static int Size(object? index) => index switch
    {
        McapChunkIndex c => checked(60 + StringSize(c.Compression) + 10 * c.MessageIndexOffsets.Count),
        McapMetadataIndex m => checked(16 + StringSize(m.Name)),
        McapAttachmentIndex a => checked(40 + StringSize(a.Name) + StringSize(a.MediaType)),
        null => 0,
        _ => throw new ArgumentException("Unsupported index.", nameof(index))
    };
    void U16(ushort value) { BinaryPrimitives.WriteUInt16LittleEndian(remaining, value); remaining = remaining[2..]; }
    void U32(uint value) { BinaryPrimitives.WriteUInt32LittleEndian(remaining, value); remaining = remaining[4..]; }
    void U64(ulong value) { BinaryPrimitives.WriteUInt64LittleEndian(remaining, value); remaining = remaining[8..]; }
    void Text(string value)
    {
        int size = Encoding.UTF8.GetByteCount(value);
        U32(checked((uint)size));
        Encoding.UTF8.GetBytes(value, remaining);
        remaining = remaining[size..];
    }
    void Offsets(IReadOnlyDictionary<ushort, ulong> offsets)
    {
        U32(checked((uint)offsets.Count * 10));
        if (offsets is Dictionary<ushort, ulong> dictionary)
        {
            foreach (var entry in dictionary) { U16(entry.Key); U64(entry.Value); }
        }
        else if (offsets is SortedList<ushort, ulong> sorted)
        {
            // Indexed access avoids both interface-enumerator boxing and a scan
            // across absent IDs. The key/value views are cached by SortedList.
            var keys = sorted.Keys;
            var values = sorted.Values;
            for (int i = 0; i < sorted.Count; i++) { U16(keys[i]); U64(values[i]); }
        }
        else
        {
            // IReadOnlyDictionary enumeration can box an enumerator. The key domain
            // is bounded, so other implementations can also avoid bridge allocations.
            for (int id = 0, left = offsets.Count; id <= ushort.MaxValue && left != 0; id++)
                if (offsets.TryGetValue((ushort)id, out var value)) { U16((ushort)id); U64(value); left--; }
        }
    }
    internal void Write(object? index)
    {
        switch (index)
        {
            case McapChunkIndex c:
                U64(c.MessageStartTime); U64(c.MessageEndTime); U64(c.ChunkStartOffset); U64(c.ChunkLength);
                Offsets(c.MessageIndexOffsets); U64(c.MessageIndexLength); Text(c.Compression);
                U64(c.CompressedSize); U64(c.UncompressedSize);
                break;
            case McapMetadataIndex m:
                U64(m.Offset); U64(m.Length); Text(m.Name);
                break;
            case McapAttachmentIndex a:
                U64(a.Offset); U64(a.Length); U64(a.LogTime); U64(a.CreateTime); U64(a.DataSize);
                Text(a.Name); Text(a.MediaType);
                break;
        }
        if (!remaining.IsEmpty) throw new InvalidOperationException("Index size changed during encoding.");
    }
}
