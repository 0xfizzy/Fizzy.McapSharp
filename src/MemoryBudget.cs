using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>A shared native storage domain. It is retained by its readers and outstanding leases.</summary>
[JsonConverter(typeof(MemoryBudgetConverter))]
public sealed class McapMemoryBudget
{
    readonly MemoryBudgetHandle handle;
    internal ulong Id { get; }
    public ulong MaxBytes { get; }
    public ulong MaxBlockBytes { get; }
    public ulong MaxRetainedBytes { get; }
    public McapMemoryBudget(ulong maxBytes = 256UL * 1024 * 1024, ulong maxBlockBytes = 64UL * 1024 * 1024, ulong maxRetainedBytes = 64UL * 1024 * 1024)
    {
        if (maxBytes == 0 || maxBlockBytes == 0 || maxBlockBytes > maxBytes || maxRetainedBytes > maxBytes)
            throw new ArgumentOutOfRangeException(nameof(maxBytes));
        Native.EnsureAvailable();
        int status = Native.fm_budget_open(checked((nuint)maxBytes), checked((nuint)maxBlockBytes), checked((nuint)maxRetainedBytes), out var p, out var id, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        handle = new(p); Id = id; MaxBytes = maxBytes; MaxBlockBytes = maxBlockBytes; MaxRetainedBytes = maxRetainedBytes;
    }
    public McapBudgetStatistics GetStatistics()
    {
        int status = Native.fm_budget_statistics(handle, out var statistics, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        return statistics;
    }
}
[StructLayout(LayoutKind.Sequential)]
public readonly record struct McapBudgetStatistics(ulong CurrentBytes, ulong PeakBytes, ulong RetainedBytes, ulong AllocationCount, ulong StorageCopyBytes);
internal sealed class MemoryBudgetConverter : JsonConverter<McapMemoryBudget>
{
    public override McapMemoryBudget Read(ref Utf8JsonReader reader, Type type, JsonSerializerOptions options) => throw new NotSupportedException();
    public override void Write(Utf8JsonWriter writer, McapMemoryBudget value, JsonSerializerOptions options)
    { writer.WriteStartObject(); writer.WriteNumber("id", value.Id); writer.WriteEndObject(); }
}
internal sealed class MemoryBudgetHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal MemoryBudgetHandle(IntPtr p) : base(true) => SetHandle(p);
    protected override bool ReleaseHandle() { Native.fm_budget_free(handle); return true; }
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_budget_open(nuint total, nuint block, nuint retained, out IntPtr handle, out ulong id, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_budget_statistics(MemoryBudgetHandle handle, out McapBudgetStatistics statistics, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fm_budget_free(IntPtr handle);
}
