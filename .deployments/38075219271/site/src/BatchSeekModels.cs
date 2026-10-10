using System.Buffers;
using System.Runtime.InteropServices;

namespace Fizzy.McapSharp;

/// <summary>One random message lookup. Index must remain undisposed during the call; Entry.Offset is relative to expanded chunk bytes.</summary>
public readonly record struct McapSeekRequest(McapPreparedChunkIndex Index, McapMessageIndexEntry Entry);
/// <summary>Cumulative snapshot diagnostics: Hits counts cache reuse and ChunkLoads counts actual chunk loads, including loads not retained by the cache. Not allocation or memory statistics.</summary>
public readonly record struct McapCacheStatistics(ulong Hits, ulong ChunkLoads);
