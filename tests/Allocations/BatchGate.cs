using Fizzy.McapSharp;

static class BatchGate
{
    public static void Run()
    {
        foreach (var compression in Enum.GetValues<McapCompression>())
        {
            using var file = new MemoryStream(32 * 1024 * 1024);
            var headers = new McapMessageHeader[16];
            var ranges = new McapPayloadRange[16];
            var payload = new byte[16 * 1024];
            using (var writer = new McapWriter(file, new() { Compression = compression, ChunkSize = 4096, CompressionThreads = 0 }, true))
            {
                var channel = writer.RegisterChannel("t", "raw");
                for (int i=0;i<16;i++) { headers[i]=new(channel,(uint)i,(ulong)i,0); ranges[i]=new((uint)(i*1024),1024); }
                for (int i=0;i<100;i++) writer.WriteBatch(headers,payload,ranges);
                var before=GC.GetAllocatedBytesForCurrentThread();
                for (int i=0;i<1000;i++) writer.WriteBatch(headers,payload,ranges);
                var allocated=GC.GetAllocatedBytesForCurrentThread()-before;
                Check("write-batch/"+compression,allocated);
                writer.Complete();
            }
            McapMessageVisitor visitor=static (in McapMessageHeader h, ReadOnlySpan<byte> p)=>p.Length==1024;
            foreach (bool borrowed in new[]{false,true})
            {
                file.Position=0;
                using var reader=McapReader.OpenMessages(file,new(){Order=McapReadOrder.File},true);
                for(int i=0;i<100;i++) { if(borrowed) reader.VisitMessages(visitor,16); else reader.ReadBatch(headers,ranges,payload); }
                var before=GC.GetAllocatedBytesForCurrentThread();
                for(int i=0;i<1000;i++) { if(borrowed) reader.VisitMessages(visitor,16); else reader.ReadBatch(headers,ranges,payload); }
                for(int i=0;i<10;i++) { if(borrowed) reader.VisitMessages(visitor,16); else reader.ReadBatch(headers,ranges,payload); }
                var allocated=GC.GetAllocatedBytesForCurrentThread()-before;
                Check((borrowed?"borrowed/":"read-batch/")+compression,allocated);
            }
        }
    }
    static void Check(string path,long bytes) { Console.WriteLine($"{path}: {bytes} B managed"); if(bytes!=0) throw new Exception($"{path} allocated {bytes} bytes"); }
}
