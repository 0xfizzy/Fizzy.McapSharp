using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

// Private wire layouts; public statistics types have no ABI layout contract.
[StructLayout(LayoutKind.Sequential)]
internal struct NativeBudgetStatistics
{
    internal ulong CurrentBytes;
    internal ulong PeakBytes;
    internal ulong RetainedBytes;
    internal ulong AllocationCount;
    internal ulong StorageCopyBytes;
    internal readonly McapBudgetStatistics ToPublic() => new(CurrentBytes, PeakBytes, RetainedBytes, AllocationCount, StorageCopyBytes);
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeResourceStatistics
{
    internal ulong CurrentBytes;
    internal ulong PeakBytes;
    internal ulong LiveBytes;
    internal ulong ReservedBytes;
    internal readonly McapResourceStatistics ToPublic() => new(CurrentBytes, PeakBytes, LiveBytes, ReservedBytes);
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeBudgetFlowStatistics
{
    internal ulong InputCopyBytes;
    internal ulong CompactionCopyBytes;
    internal ulong DeliveryCopyBytes;
    internal ulong OtherCopyBytes;
    internal ulong EncodedInputBytes;
    internal ulong EncodedOutputBytes;
    internal ulong DecodedInputBytes;
    internal ulong DecodedOutputBytes;
    internal ulong DecompressionsStarted;
    internal ulong DecompressionsCompleted;
    internal ulong CacheHits;
    internal ulong CacheMisses;
    internal ulong CacheEvictions;
    internal ulong ReclaimedBytes;
    internal readonly McapBudgetFlowStatistics ToPublic() => new(InputCopyBytes, CompactionCopyBytes, DeliveryCopyBytes, OtherCopyBytes, EncodedInputBytes, EncodedOutputBytes, DecodedInputBytes, DecodedOutputBytes, DecompressionsStarted, DecompressionsCompleted, CacheHits, CacheMisses, CacheEvictions) { ReclaimedBytes = ReclaimedBytes };
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeDetailedBudgetStatistics
{
    internal NativeResourceStatistics Input;
    internal NativeResourceStatistics Decompressed;
    internal NativeResourceStatistics Writer;
    internal NativeResourceStatistics CodecEncoder;
    internal NativeResourceStatistics CodecDecoder;
    internal NativeResourceStatistics Index;
    internal NativeResourceStatistics Descriptor;
    internal NativeResourceStatistics Declaration;
    internal NativeResourceStatistics Scratch;
    internal ulong AllocationCount;
    internal ulong AllocatedBytes;
    internal ulong BudgetRejections;
    internal NativeBudgetFlowStatistics Flow;
    internal ulong ActiveLeasePayloadBytes;
    internal ulong CachedPayloadBytes;
    internal ulong CurrentBytes;
    internal ulong PeakBytes;
    internal ulong IdleBytes;
    internal ulong ReallocationCount;
    internal ulong ImmediatelyReclaimableBytes;
    internal ulong MappedLogicalBytes;
    internal readonly McapDetailedBudgetStatistics ToPublic() => new(
        Input.ToPublic(),
        Decompressed.ToPublic(),
        Writer.ToPublic(),
        CodecEncoder.ToPublic(),
        CodecDecoder.ToPublic(),
        Index.ToPublic(),
        Descriptor.ToPublic(),
        Declaration.ToPublic(),
        Scratch.ToPublic(),
        AllocationCount,
        AllocatedBytes,
        BudgetRejections,
        Flow.ToPublic(),
        ActiveLeasePayloadBytes,
        CachedPayloadBytes,
        CurrentBytes,
        PeakBytes,
        IdleBytes) { ReallocationCount = ReallocationCount, ImmediatelyReclaimableBytes = ImmediatelyReclaimableBytes, MappedLogicalBytes = MappedLogicalBytes };
}
