using Xunit;

namespace Fizzy.McapSharp.Tests;

public class LeaseStorageTests
{
    [Theory]
    [InlineData(McapCompression.None)] [InlineData(McapCompression.Lz4)] [InlineData(McapCompression.Zstd)]
    public unsafe void DuplicatePointersSurviveCacheEvictionAndSnapshotDisposal(McapCompression compression)
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        try
        {
            using (var writer = new McapWriter(path, new() { Compression = compression, CompressionThreads = 0, ChunkSize = 1024 }))
            {
                var channel = writer.RegisterChannel("t", "raw");
                var payload = new byte[8192];
                for (uint i = 0; i < 3; i++) { Array.Fill(payload, (byte)(i + 1)); writer.WriteMessage(new(channel, i, i, 0), payload); }
                writer.Complete();
            }
            using var snapshot = McapIndexSnapshot.OpenMapped(path, new() { MaxRandomAccessCacheBytes = 16000 });
            var chunks = snapshot.GetSummary()!.ChunkIndexes;
            using var first = new McapPreparedChunkIndex(chunks[0]);
            using var second = new McapPreparedChunkIndex(chunks[1]);
            var a = snapshot.ReadMessageIndexes(first)[0].Records[0];
            var b = snapshot.ReadMessageIndexes(second)[0].Records[0];
            using var batch = snapshot.SeekMessages([new(first, a), new(first, a)]);
            Assert.Equal(1UL, snapshot.GetCacheStatistics().ChunkLoads);
            fixed (byte* original = batch.GetPayload(0)) fixed (byte* duplicate = batch.GetPayload(1))
            {
                Assert.Equal((nuint)original, (nuint)duplicate);
                using var retained = batch.RetainMessage(0);
                using (var evict = snapshot.SeekMessages([new(second, b)])) Assert.Equal(2, evict.GetPayload(0)[0]);
                using (var reload = snapshot.SeekMessages([new(first, a)])) Assert.Equal(1, reload.GetPayload(0)[0]);
                Assert.Equal(3UL, snapshot.GetCacheStatistics().ChunkLoads); // First chunk was actually evicted.
                snapshot.Dispose(); batch.Dispose();
                fixed (byte* stillValid = retained.Payload) Assert.Equal((nuint)original, (nuint)stillValid);
                Assert.Equal(8192, retained.Payload.Length); Assert.Equal(1, retained.Payload[8191]);
            }
        }
        finally { File.Delete(path); }
    }
}
