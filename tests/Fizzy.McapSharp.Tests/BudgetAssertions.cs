using Xunit;
namespace Fizzy.McapSharp.Tests;

// Operation lifetime tests compare against the persistent budget control storage.
// Native identity tests independently verify the control's physical Layout.
internal static class BudgetAssertions
{
    static readonly McapDetailedBudgetStatistics Initial = new McapMemoryBudget(maxRetainedBytes: 0).GetDetailedStatistics();
    internal static ulong ControlBytes => Initial.CurrentBytes;
    internal static ulong ControlAllocations => Initial.AllocationCount;
    internal static void Idle(McapMemoryBudget budget)
    {
        var actual = budget.GetDetailedStatistics();
        Assert.Equal(ControlBytes, actual.CurrentBytes);
        Assert.Equal(ControlBytes, actual.Scratch.LiveBytes);
        Assert.Equal(0UL, actual.Scratch.ReservedBytes);
        Assert.Equal(0UL, actual.Input.CurrentBytes + actual.Decompressed.CurrentBytes + actual.Writer.CurrentBytes +
            actual.CodecEncoder.CurrentBytes + actual.CodecDecoder.CurrentBytes + actual.Index.CurrentBytes +
            actual.Descriptor.CurrentBytes + actual.Declaration.CurrentBytes);
        Assert.Equal(0UL, actual.IdleBytes);
    }
}
