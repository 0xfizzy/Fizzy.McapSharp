import json, pathlib, sys
from mcap.reader import make_reader
from mcap.writer import Writer, CompressionType
root=pathlib.Path(sys.argv[1]); root.mkdir(parents=True, exist_ok=True)
paths = list(root.glob('dotnet-*.mcap'))
assert len(paths) == 3, 'Expected all three .NET compression fixtures'
for path in paths:
    with path.open('rb') as stream:
        reader=make_reader(stream,validate_crcs=True)
        messages=list(reader.iter_messages(topics=['/test'],start_time=200,end_time=500))
        assert len(messages)==3 and [m.sequence for _,_,m in messages]==[2,3,4]
        assert json.loads(messages[0][2].data)=={'n':2}
        assert list(reader.iter_metadata())[0].metadata['origin']=='dotnet'
        assert list(reader.iter_attachments())[0].data==b'attachment'
    print('Validated',path.name)
for compression in CompressionType:
    with (root/f'python-{compression.name}.mcap').open('wb') as stream:
        writer=Writer(stream,compression=compression,chunk_size=64,enable_crcs=True,enable_data_crcs=True)
        writer.start()
        schema=writer.register_schema('sample','jsonschema',b'{}')
        channel=writer.register_channel('/test','json',schema)
        for n in range(100): writer.add_message(channel,n*100,json.dumps({'n':n}).encode(),n*100+1,n)
        writer.add_metadata('session',{'origin':'python'})
        writer.add_attachment(100,90,'sample','text/plain',b'attachment')
        writer.finish()
