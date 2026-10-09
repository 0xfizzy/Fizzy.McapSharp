namespace Fizzy.McapSharp;

/// <summary>A record model. Results returned by <see cref="McapRecords.Parse"/> are independently owned. Pattern match the concrete record type to access its fields. Unknown opcodes return <see cref="McapRawRecord"/>; no payload or collection borrows the input span.</summary>
public interface IMcapRecord { }

