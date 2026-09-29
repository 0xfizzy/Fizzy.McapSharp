using Fizzy.McapSharp;
var folder = Path.GetFullPath(args[1]); Directory.CreateDirectory(folder);
if (args[0] == "write")
{
    foreach (var compression in Enum.GetValues<McapCompression>())
    {
        var path=Path.Combine(folder,$"dotnet-{compression}.mcap");
        using var writer=new McapWriter(path,new(){Compression=compression,ChunkSize=64});
        var schema=writer.RegisterSchema("sample","jsonschema","{}"u8);
        var channel=writer.RegisterChannel("/test","json",schema);
        for(uint n=0;n<100;n++) writer.WriteMessage(channel,n*100,n*100+1,n,System.Text.Encoding.UTF8.GetBytes($"{{\"n\":{n}}}"));
        writer.WriteMetadata("session",new Dictionary<string,string>{{"origin","dotnet"}});
        writer.WriteAttachment("sample","text/plain",100,90,"attachment"u8);
        writer.Complete();
    }
}
else if (args[0] is "read" or "read-all")
{
    var paths = Directory.GetFiles(folder, args[0] == "read-all" ? "*.mcap" : "python-*.mcap", SearchOption.AllDirectories);
    if (paths.Length != (args[0] == "read-all" ? 18 : 3)) throw new Exception("Missing interoperability fixtures");
    foreach(var path in paths)
    {
        var reader=new McapReader(path); reader.Validate();
        var messages=reader.ReadMessages(new(){Topic="/test",StartTime=200,EndTime=500}).ToArray();
        if(messages.Length!=3 || messages[0].Sequence!=2 || messages[2].Sequence!=4) throw new Exception("Interoperability query failed: "+path);
        if(reader.ReadMetadata().Single().Values["origin"]!=(Path.GetFileName(path).StartsWith("python-") ? "python" : "dotnet")) throw new Exception("Metadata mismatch");
        if(System.Text.Encoding.UTF8.GetString(reader.ReadAttachments().Single().Data)!="attachment") throw new Exception("Attachment mismatch");
        Console.WriteLine("Validated "+Path.GetFileName(path));
    }
}

else throw new ArgumentException("Expected write, read or read-all");
