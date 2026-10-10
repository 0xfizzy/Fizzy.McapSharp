namespace Fizzy.McapSharp;

/// <summary>Schema declaration. Reader results own independent mutable schema bytes; direct construction retains the supplied array.</summary>
public sealed record McapSchema(ushort Id, string Name, string Encoding, byte[] Data) : IMcapRecord;

/// <summary>Resolved channel declaration with optional schema and metadata. Reader results own their data; direct construction retains supplied references.</summary>
public sealed record McapChannel(ushort Id, string Topic, string MessageEncoding, McapSchema? Schema, IReadOnlyDictionary<string, string> Metadata);
