using System.Buffers.Binary;
using System.Text;

namespace Fizzy.McapSharp;

public enum McapOpcode : byte { Header = 1, Footer, Schema, Channel, Message, Chunk, MessageIndex, ChunkIndex, Attachment, AttachmentIndex, Statistics, Metadata, MetadataIndex, SummaryOffset, DataEnd }
public readonly record struct McapFooter(ulong SummaryStart, ulong SummaryOffsetStart, uint SummaryCrc);
public readonly record struct McapSummaryOffset(byte GroupOpcode, ulong GroupStart, ulong GroupLength);
public readonly record struct McapDataEnd(uint DataSectionCrc);
public static class McapFormat
{
    public static ReadOnlySpan<byte> Magic => [0x89, 0x4d, 0x43, 0x41, 0x50, 0x30, 0x0d, 0x0a];
    public const string RustVersion = "0.25.0";
    public const string LibraryIdentifier = "mcap-rust/" + RustVersion;
}

/// <summary>A validated record body over caller-owned memory. UTF-8 and collection fields remain spans.</summary>
public readonly ref struct McapRecordView
{
    public byte Opcode { get; }
    public ReadOnlySpan<byte> Data { get; }
    McapRecordView(byte opcode, ReadOnlySpan<byte> data) { Opcode = opcode; Data = data; }
    public static unsafe McapRecordView Parse(byte opcode, ReadOnlySpan<byte> body)
    {
        Native.EnsureAvailable();
        fixed (byte* p = body)
        {
            int status = Native.fm_parse_record(opcode, p, (nuint)body.Length, out var r);
            if (status < 0) throw Native.ConsumeError(r);
        }
        return new(opcode, body);
    }
    void Require(byte opcode) { if (Opcode != opcode) throw new InvalidOperationException("Wrong record type."); }
    public McapFooter Footer { get { Require(2); var r = Fields; return new(r.ReadUInt64(), r.ReadUInt64(), r.ReadUInt32()); } }
    public McapMessageHeader MessageHeader { get { Require(5); var r = Fields; return new(r.ReadUInt16(), r.ReadUInt32(), r.ReadUInt64(), r.ReadUInt64()); } }
    public ReadOnlySpan<byte> MessageData { get { Require(5); return Data[22..]; } }
    public McapSummaryOffset SummaryOffset { get { Require(14); var r = Fields; return new(r.ReadByte(), r.ReadUInt64(), r.ReadUInt64()); } }
    public McapDataEnd DataEnd { get { Require(15); return new(BinaryPrimitives.ReadUInt32LittleEndian(Data)); } }
    public McapRecordFields Fields => new(Data);
    public McapRecord ToOwned() => new(Opcode, Data.ToArray());
}

/// <summary>Allocation-free typed field cursor. Length-prefixed collections can be traversed with another cursor.</summary>
public ref struct McapRecordFields
{
    ReadOnlySpan<byte> data;
    public McapRecordFields(ReadOnlySpan<byte> data) => this.data = data;
    public bool IsEmpty => data.IsEmpty;
    public ReadOnlySpan<byte> Remaining => data;
    public ReadOnlySpan<byte> ReadBytes(int length)
    {
        if ((uint)length > (uint)data.Length) throw new McapException("Invalid record field length.");
        var value = data[..length]; data = data[length..]; return value;
    }
    public byte ReadByte() => ReadBytes(1)[0];
    public ushort ReadUInt16() => BinaryPrimitives.ReadUInt16LittleEndian(ReadBytes(2));
    public uint ReadUInt32() => BinaryPrimitives.ReadUInt32LittleEndian(ReadBytes(4));
    public ulong ReadUInt64() => BinaryPrimitives.ReadUInt64LittleEndian(ReadBytes(8));
    public ReadOnlySpan<byte> ReadUtf8() => ReadBytes(checked((int)ReadUInt32()));
    public string ReadString() => Encoding.UTF8.GetString(ReadUtf8());
    public McapRecordFields ReadCollection() => new(ReadBytes(checked((int)ReadUInt32())));
}
