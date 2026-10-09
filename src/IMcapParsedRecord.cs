namespace Fizzy.McapSharp;

/// <summary>An independently owned result of <see cref="McapRecords.Parse"/>. Pattern match the concrete record type to access its fields. Unknown opcodes return <see cref="McapRawRecord"/>; no payload or collection borrows the input span.</summary>
public interface IMcapParsedRecord { }
