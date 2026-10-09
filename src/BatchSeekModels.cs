using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

public readonly record struct McapSeekRequest(McapPreparedChunkIndex Index, McapMessageIndexEntry Entry);
public readonly record struct McapCacheStatistics(ulong Hits, ulong ChunkLoads);
