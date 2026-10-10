using System.Buffers.Binary;
using System.Text;

namespace Fizzy.McapSharp;

/// <summary>Standard MCAP record opcodes. Private records use byte values 0x80-0xFF.</summary>
public enum McapOpcode : byte
{
    /// <summary>File header declaring profile and library.</summary>
    Header = 1,
    /// <summary>File footer with summary locations and checksum.</summary>
    Footer,
    /// <summary>Schema declaration.</summary>
    Schema,
    /// <summary>Channel declaration.</summary>
    Channel,
    /// <summary>Message header and payload.</summary>
    Message,
    /// <summary>Compressed or uncompressed record chunk.</summary>
    Chunk,
    /// <summary>Per-channel message locations within a chunk.</summary>
    MessageIndex,
    /// <summary>Chunk location and sizes.</summary>
    ChunkIndex,
    /// <summary>Named attachment payload and checksum.</summary>
    Attachment,
    /// <summary>Attachment location and fields.</summary>
    AttachmentIndex,
    /// <summary>Recording counts and message time range.</summary>
    Statistics,
    /// <summary>Named string metadata.</summary>
    Metadata,
    /// <summary>Metadata location and length.</summary>
    MetadataIndex,
    /// <summary>Summary record-group location and length.</summary>
    SummaryOffset,
    /// <summary>Data-section end and checksum.</summary>
    DataEnd,
}
/// <summary>Footer fields: summary offsets are byte positions from the MCAP origin (the initial Stream position for Stream inputs); SummaryCrc is the stored summary checksum.</summary>
public readonly record struct McapFooter(ulong SummaryStart, ulong SummaryOffsetStart, uint SummaryCrc) : IMcapRecord;
/// <summary>Summary group descriptor with byte start from the MCAP origin (the initial Stream position for Stream inputs) and byte length.</summary>
public readonly record struct McapSummaryOffset(byte GroupOpcode, ulong GroupStart, ulong GroupLength) : IMcapRecord;
/// <summary>Data-section end marker containing its stored CRC.</summary>
public readonly record struct McapDataEnd(uint DataSectionCrc) : IMcapRecord;
/// <summary>Constants identifying the MCAP wire format and bundled upstream implementation.</summary>
public static class McapFormat
{
    /// <summary>Eight bytes framing an MCAP file.</summary>
    public static ReadOnlySpan<byte> Magic => [0x89, 0x4d, 0x43, 0x41, 0x50, 0x30, 0x0d, 0x0a];
    /// <summary>Version of the bundled official Rust MCAP crate.</summary>
    public const string RustVersion = "0.25.0";
    /// <summary>Default upstream library identifier written to the file header.</summary>
    public const string LibraryIdentifier = "mcap-rust/" + RustVersion;
}

/// <summary>A record body validated at Parse time over caller-owned memory. Keep the underlying bytes unchanged for the entire lifetime of this view and all derived field cursors or spans. Read-only spans do not prevent mutation through other aliases. UTF-8 and collection fields remain borrowed spans; ToOwned creates an independent copy.</summary>
public readonly ref struct McapRecordView
{
    /// <summary>Opcode associated with this body; unknown values are preserved.</summary>
    public byte Opcode { get; }
    /// <summary>Borrowed record body excluding opcode and length. Keep the underlying bytes unchanged.</summary>
    public ReadOnlySpan<byte> Data { get; }
    McapRecordView(byte opcode, ReadOnlySpan<byte> data) { Opcode = opcode; Data = data; }
    /// <summary>Validates the current body bytes with the upstream parser and borrows them without copying. Keep the source unchanged until all derived views and spans are no longer used.</summary>
    public static unsafe McapRecordView Parse(byte opcode, ReadOnlySpan<byte> body)
    {
        Native.EnsureAvailable();
        fixed (byte* p = body)
        {
            int status = Native.fm_parse_record(opcode, p, (nuint)body.Length, out var r);
            if (status < Protocol.Status.Success) throw Native.ConsumeError(r);
        }
        return new(opcode, body);
    }
    void Require(byte opcode) { if (Opcode != opcode) throw new InvalidOperationException("Wrong record type."); }
    /// <summary>Decodes footer fields; throws InvalidOperationException for another opcode.</summary>
    public McapFooter Footer { get { Require(2); var r = Fields; return new(r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt32()); } }
    /// <summary>Decodes message fields; throws InvalidOperationException for another opcode.</summary>
    public McapMessageHeader MessageHeader { get { Require(5); var r = Fields; return new(r.ReadUInt16(), r.ReadUInt32(), r.ReadUInt64(), r.ReadUInt64()); } }
    /// <summary>Borrows message payload bytes; throws InvalidOperationException for another opcode.</summary>
    public ReadOnlySpan<byte> MessageData { get { Require(5); return Data[22..]; } }
    /// <summary>Decodes summary-offset fields; throws InvalidOperationException for another opcode.</summary>
    public McapSummaryOffset SummaryOffset { get { Require(14); var r = Fields; return new(r.ReadByte(), r.ReadUInt64(), r.ReadUInt64()); } }
    /// <summary>Decodes the data-section checksum; throws InvalidOperationException for another opcode.</summary>
    public McapDataEnd DataEnd { get { Require(15); return new(BinaryPrimitives.ReadUInt32LittleEndian(Data)); } }
    /// <summary>Creates a borrowed cursor at the start of this body. Field order depends on Opcode.</summary>
    public McapRecordFields Fields => new(Data);
    /// <summary>Copies the body into an independent mutable array with the same opcode.</summary>
    public McapRawRecord ToOwned() => new(Opcode, Data.ToArray());
}

/// <summary>Allocation-free typed field cursor. Length-prefixed collections can be traversed with another cursor.</summary>
public ref struct McapRecordFields
{
    ReadOnlySpan<byte> data;
    /// <summary>Creates a cursor borrowing the supplied bytes without record validation. Keep the source unchanged during use.</summary>
    public McapRecordFields(ReadOnlySpan<byte> data) => this.data = data;
    /// <summary>True when no unread bytes remain.</summary>
    public bool IsEmpty => data.IsEmpty;
    /// <summary>Borrowed unread bytes, without advancing the cursor.</summary>
    public ReadOnlySpan<byte> Remaining => data;
    /// <summary>Borrows and advances by exactly length bytes; rejects negative lengths or lengths beyond the remaining body.</summary>
    public ReadOnlySpan<byte> ReadBytes(int length)
    {
        if ((uint)length > (uint)data.Length) throw new McapException("Invalid record field length.");
        var value = data[..length]; data = data[length..]; return value;
    }
    /// <summary>Reads one byte and advances the cursor.</summary>
    public byte ReadByte() => ReadBytes(1)[0];
    /// <summary>Reads a little-endian unsigned 16-bit field and advances the cursor.</summary>
    public ushort ReadUInt16() => BinaryPrimitives.ReadUInt16LittleEndian(ReadBytes(2));
    /// <summary>Reads a little-endian unsigned 32-bit field and advances the cursor.</summary>
    public uint ReadUInt32() => BinaryPrimitives.ReadUInt32LittleEndian(ReadBytes(4));
    /// <summary>Reads a little-endian unsigned 64-bit field and advances the cursor.</summary>
    public ulong ReadUInt64() => BinaryPrimitives.ReadUInt64LittleEndian(ReadBytes(8));
    /// <summary>Borrows a 32-bit byte-length-prefixed UTF-8 field and advances the cursor; no string allocation.</summary>
    public ReadOnlySpan<byte> ReadUtf8() => ReadBytes(checked((int)ReadUInt32()));
    /// <summary>Decodes a length-prefixed UTF-8 field into an owned string and advances the cursor.</summary>
    public string ReadString() => Encoding.UTF8.GetString(ReadUtf8());
    /// <summary>Borrows a 32-bit byte-length-prefixed collection into its own cursor and advances the parent.</summary>
    public McapRecordFields ReadCollection() => new(ReadBytes(checked((int)ReadUInt32())));
}
