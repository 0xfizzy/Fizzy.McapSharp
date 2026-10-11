namespace Fizzy.McapSharp;

/// <summary>Owned named string metadata record.</summary>
public sealed record McapMetadata(string Name, IReadOnlyDictionary<string, string> Values) : IMcapRecord;

/// <summary>Owned attachment payload and fields. Times use caller-defined nanoseconds.</summary>
public sealed record McapAttachment(string Name, string MediaType, ulong LogTime, ulong CreateTime, byte[] Data);

/// <summary>An independently owned opcode and unparsed record body. Data excludes the opcode and length prefix. Use McapRecords.Parse for typed fields.</summary>
public sealed record McapRawRecord(byte Opcode, byte[] Data) : IMcapRecord;
