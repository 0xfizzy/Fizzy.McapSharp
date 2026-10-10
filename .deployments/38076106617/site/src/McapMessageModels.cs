namespace Fizzy.McapSharp;

/// <summary>Message descriptor. LogTime and PublishTime are caller-defined nanoseconds; ChannelId refers to a declared channel.</summary>
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
public readonly record struct McapMessageHeader(ushort ChannelId, uint Sequence, ulong LogTime, ulong PublishTime);

/// <summary>Message with resolved declarations and payload bytes. Reader results own independent data; direct construction retains supplied references. Times use caller-defined nanoseconds.</summary>
public sealed record McapMessage(McapChannel Channel, ulong LogTime, ulong PublishTime, uint Sequence, byte[] Data);
