using Xunit;
namespace Fizzy.McapSharp.Tests;
public class WriterRecoveryTests
{
    public static IEnumerable<object[]> Cases()
    {
        foreach (int bit in new[] { 1, 2, 4, 8, 16 })
        foreach (int mask in new[] { 31, 0, bit, 31 ^ bit })
        foreach (bool prepared in new[] { false, true })
        foreach (bool chunks in new[] { false, true })
        foreach (bool file in new[] { false, true })
        foreach (var compression in Enum.GetValues<McapCompression>())
            yield return new object[] { bit, mask, prepared, chunks, file, compression };
    }
    [Theory, MemberData(nameof(Cases))]
    public void RejectedOperationCanBeCorrected(int bit, int mask, bool prepared, bool chunks, bool file, McapCompression compression)
    {
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        using var stream = new ObservedStream();
        try
        {
            var options = new McapWriterOptions { RecoverableErrors = (McapRecoverableWriterErrors)mask, UseChunks = chunks, Compression = compression, ChunkSize = 32 };
            using (var w = file ? new McapWriter(path, options) : new McapWriter(stream, options, true))
            {
                w.RegisterSchema(1, "s", "raw", []);
                w.RegisterChannel(1, "t", "raw", 1);
                w.WriteMessage(new McapMessageHeader(1, 0, 10, 10), [1]);
                var writes = stream.Writes; var seeks = stream.Seeks;
                var e = Assert.Throws<McapException>(() => Reject(w, bit, prepared));
                Assert.Equal(bit switch { 1 => McapErrorKind.InvalidSchemaId, 2 => McapErrorKind.ConflictingSchemas, 4 => McapErrorKind.UnknownSchema, 8 => McapErrorKind.ConflictingChannels, _ => McapErrorKind.UnknownChannel }, e.Kind);
                Assert.Equal(writes, stream.Writes); Assert.Equal(seeks, stream.Seeks);
                Assert.Equal((mask & bit) != 0, e.CanContinueWriting);
                if (!e.CanContinueWriting) { Assert.Throws<InvalidOperationException>(() => w.Complete()); return; }
                w.RegisterSchema(2, "new", "raw", []);
                w.RegisterChannel(2, "new", "raw", 2);
                w.WriteMessage(new McapMessageHeader(2, 1, 20, 20), [2]);
                w.Complete();
                Assert.Equal(2ul, w.GetSummary().Statistics!.MessageCount);
            }
            using Stream input = file ? File.OpenRead(path) : new MemoryStream(stream.ToArray());
            using var reader = McapFileReader.OpenMessages(input, options: McapReaderOptions.Strict);
            Assert.Equal(new ulong[] { 10, 20 }, reader.ReadMessages().Select(m => m.LogTime));
            Assert.True(reader.IsFullyValidated);
            Assert.Equal(2, reader.GetSummary()!.SchemaIds.Count);
        }
        finally { if (File.Exists(path)) File.Delete(path); }
    }
    static void Reject(McapWriter w, int bit, bool prepared)
    {
        if (bit == 16) { w.WriteMessage(new McapMessageHeader(2, 99, 999, 999), []); return; }
        if (prepared)
        {
            using var op = bit switch {
                1 => McapPreparedOperation.Schema("s", "raw", [], 0),
                2 => McapPreparedOperation.Schema("conflict", "raw", [], 1),
                4 => McapPreparedOperation.Channel("new", "raw", 2),
                _ => McapPreparedOperation.Channel("conflict", "raw", 1, id: 1)
            };
            w.WritePrepared(op); return;
        }
        switch (bit) {
            case 1: w.RegisterSchema(0, "s", "raw", []); break;
            case 2: w.RegisterSchema(1, "conflict", "raw", []); break;
            case 4: w.RegisterChannel(2, "new", "raw", 2); break;
            case 8: w.RegisterChannel(1, "conflict", "raw", 1); break;
        }
    }
    [Fact]
    public void DefaultsAndInvalidFlags()
    {
        Assert.Equal(31, (int)new McapWriterOptions().RecoverableErrors);
        var path = Path.Combine(Path.GetTempPath(), Guid.NewGuid() + ".mcap");
        var options = new McapWriterOptions { RecoverableErrors = (McapRecoverableWriterErrors)32 };
        Assert.Throws<ArgumentOutOfRangeException>(() => new McapWriter(path, options));
        Assert.False(File.Exists(path));
        using var s = new MemoryStream();
        Assert.Throws<ArgumentOutOfRangeException>(() => new McapWriter(s, options));
        Assert.True(s.CanWrite); Assert.Equal(0, s.Length);
        using var w = new McapWriter(s);
        Assert.True(Assert.Throws<McapException>(() => w.WriteMessage(new McapMessageHeader(9, 0, 0, 0), [])).CanContinueWriting);
        w.Complete();
    }
    [Fact]
    public void CallbackExceptionCannotAuthorizeRecovery()
    {
        using var s = new ObservedStream();
        using var w = new McapWriter(s, new() { UseChunks = false }, true);
        var captured = Assert.Throws<McapException>(() => w.RegisterSchema(0, "s", "raw", []));
        Assert.True(captured.CanContinueWriting);
        s.Failure = captured;
        Assert.Same(captured, Assert.Throws<McapException>(() => w.RegisterChannel("t", "raw")));
        Assert.Throws<InvalidOperationException>(() => w.Complete());
    }
    [Fact]
    public void FullMessageConflictIsTerminal()
    {
        using var s = new MemoryStream(); using var w = new McapWriter(s);
        w.RegisterChannel(1, "old", "raw");
        var e = Assert.Throws<McapException>(() => w.WriteMessage(new McapMessage(new(1, "new", "raw", null, new Dictionary<string,string>()), 0, 0, 0, [])));
        Assert.False(e.CanContinueWriting);
        Assert.Throws<InvalidOperationException>(() => w.Complete());
    }
    sealed class ObservedStream : MemoryStream
    {
        public int Writes, Seeks;
        public Exception? Failure;
        public override void Write(ReadOnlySpan<byte> bytes) { Writes++; if (Failure is not null) { if (!bytes.IsEmpty) base.Write(bytes[..1]); throw Failure; } base.Write(bytes); }
        public override long Seek(long offset, SeekOrigin origin) { Seeks++; return base.Seek(offset, origin); }
    }
}
