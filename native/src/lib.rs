//! Private ABI v1. Handles must originate here, be freed once, and never be used concurrently.
//! All strings are length-delimited UTF-8; all output buffers are owned until fm_buffer_free.
use base64::Engine;
use mcap::{read, records, sans_io};
use memmap2::Mmap;
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs::File,
    io::Read,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};
type Error = Box<dyn std::error::Error>;
type Outcome<T> = Result<T, Error>;

#[repr(C)]
#[derive(Default)]
pub struct Response {
    json: *mut u8,
    json_len: usize,
    data: *mut u8,
    data_len: usize,
    value: u64,
}
fn buffer(bytes: Vec<u8>) -> (*mut u8, usize) {
    if bytes.is_empty() {
        return (ptr::null_mut(), 0);
    }
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed) as *mut u8, len)
}
fn respond(out: &mut Response, header: Vec<u8>, data: Vec<u8>, value: u64) {
    (out.json, out.json_len) = buffer(header);
    (out.data, out.data_len) = buffer(data);
    out.value = value;
}
fn guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    if out.is_null() {
        return -1;
    }
    let out = unsafe { &mut *out };
    *out = Response::default();
    match catch_unwind(AssertUnwindSafe(|| f(out))) {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            respond(out, e.to_string().into_bytes(), vec![], 0);
            -1
        }
        Err(_) => {
            respond(
                out,
                b"Native MCAP panic; operation failed".to_vec(),
                vec![],
                0,
            );
            -1
        }
    }
}
unsafe fn bytes<'a>(p: *const u8, n: usize) -> Outcome<&'a [u8]> {
    if n == 0 {
        return Ok(&[]);
    }
    if p.is_null() || n > isize::MAX as usize {
        return Err("Invalid input buffer".into());
    }
    Ok(slice::from_raw_parts(p, n))
}
unsafe fn request(p: *const u8, n: usize) -> Outcome<Value> {
    Ok(serde_json::from_slice(bytes(p, n)?)?)
}
fn string<'a>(v: &'a Value, key: &str) -> Outcome<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("Missing string: {key}").into())
}
fn number(v: &Value, key: &str) -> Outcome<u64> {
    v[key]
        .as_u64()
        .ok_or_else(|| format!("Missing integer: {key}").into())
}
fn map(v: &Value) -> Outcome<BTreeMap<String, String>> {
    Ok(serde_json::from_value(v.clone())?)
}
#[no_mangle]
pub extern "C" fn fm_abi_version() -> u32 {
    1
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(p, n)));
    }
}
pub struct Writer {
    inner: Option<mcap::Writer<File>>,
    failed: bool,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(w) = self.inner.take() {
            drop(w.into_inner());
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_open(
    p: *const u8,
    n: usize,
    handle: *mut *mut Writer,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null handle output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let compression = match string(&v, "compression")? {
            "none" => None,
            "lz4" => Some(mcap::Compression::Lz4),
            "zstd" => Some(mcap::Compression::Zstd),
            _ => return Err("Unknown compression".into()),
        };
        let indexes = v["indexes"].as_bool().unwrap_or(true);
        let options = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(Some(number(&v, "chunk_size")?))
            .use_chunks(v["use_chunks"].as_bool().unwrap_or(true))
            .profile(string(&v, "profile")?)
            .library("Fizzy.McapSharp 0.1.0 / mcap-rust 0.25.0")
            .emit_summary_records(indexes)
            .emit_message_indexes(indexes)
            .emit_chunk_indexes(indexes)
            .calculate_chunk_crcs(true)
            .calculate_data_section_crc(true)
            .calculate_summary_section_crc(true)
            .calculate_attachment_crcs(true);
        // Never overwrite an existing recording accidentally.
        let file = File::options()
            .write(true)
            .read(true)
            .create_new(true)
            .open(string(&v, "path")?)?;
        let writer = options.create(file)?;
        *handle = Box::into_raw(Box::new(Writer {
            inner: Some(writer),
            failed: false,
        }));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_call(
    handle: *mut Writer,
    op: u32,
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |out| {
        let holder = handle.as_mut().ok_or("Null writer")?;
        if holder.failed {
            return Err("Writer has failed".into());
        }
        let v = request(p, n)?;
        let payload = bytes(data, len)?;
        let w = holder.inner.as_mut().ok_or("Writer already completed")?;
        let value = match op {
            1 => w.add_schema(string(&v, "name")?, string(&v, "encoding")?, payload)? as u64,
            2 => w.add_channel(
                number(&v, "schema_id")?.try_into()?,
                string(&v, "topic")?,
                string(&v, "encoding")?,
                &map(&v["metadata"])?,
            )? as u64,
            3 => {
                w.write_to_known_channel(
                    &records::MessageHeader {
                        channel_id: number(&v, "channel_id")?.try_into()?,
                        sequence: number(&v, "sequence")?.try_into()?,
                        log_time: number(&v, "log_time")?,
                        publish_time: number(&v, "publish_time")?,
                    },
                    payload,
                )?;
                0
            }
            4 => {
                w.write_metadata(&records::Metadata {
                    name: string(&v, "name")?.into(),
                    metadata: map(&v["metadata"])?,
                })?;
                0
            }
            5 => {
                w.attach(&mcap::Attachment {
                    name: string(&v, "name")?.into(),
                    media_type: string(&v, "media_type")?.into(),
                    log_time: number(&v, "log_time")?,
                    create_time: number(&v, "create_time")?,
                    data: Cow::Borrowed(payload),
                })?;
                0
            }
            6 => {
                w.flush()?;
                0
            }
            7 => {
                w.finish()?;
                let file = holder.inner.take().unwrap().into_inner();
                file.sync_all()?;
                0
            }
            _ => return Err("Unknown writer operation".into()),
        };
        out.value = value;
        Ok(0)
    });
    if status < 0 {
        if let Some(w) = handle.as_mut() {
            w.failed = true;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_free(handle: *mut Writer) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(handle))));
    }
}
fn open_map(path: &str) -> Outcome<(File, Mmap)> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let file = options.open(path)?;
    // File is kept open for the mapping lifetime. Only Windows denies ordinary writes/deletion.
    // On every platform the caller must keep the mapped file unchanged (including its length).
    let mmap = unsafe { Mmap::map(&file)? };
    Ok((file, mmap))
}
fn schema_json(s: &mcap::Schema) -> Value {
    json!({"id":s.id,"name":s.name,"encoding":s.encoding,"data":base64::engine::general_purpose::STANDARD.encode(&s.data)})
}
fn channel_json(c: &mcap::Channel) -> Value {
    json!({"id":c.id,"topic":c.topic,"messageEncoding":c.message_encoding,"schema":c.schema.as_ref().map(|s|schema_json(s)),"metadata":c.metadata})
}
fn message_output(m: mcap::Message) -> (Value, Vec<u8>) {
    (
        json!({"channel":channel_json(&m.channel),"sequence":m.sequence,"logTime":m.log_time,"publishTime":m.publish_time}),
        m.data.into_owned(),
    )
}
type RecordStream = Box<dyn Iterator<Item = Outcome<(Value, Vec<u8>)>>>;
pub struct Reader {
    // Drop order matters: the iterator borrows stable map/summary allocations and is dropped first.
    stream: RecordStream,
    _summary: Box<Option<mcap::Summary>>,
    _mapping: Mmap,
    _file: File,
    failed: bool,
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_open(
    p: *const u8,
    n: usize,
    handle: *mut *mut Reader,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        if handle.is_null() {
            return Err("Null handle output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let (file, mapping) = open_map(string(&v, "path")?)?;
        let mode = string(&v, "mode")?;
        let recovery = v["recovery"].as_bool().unwrap_or(false);
        let summary = Box::new(if recovery {
            None
        } else {
            mcap::Summary::read(&mapping)?
        });
        // These references never escape Reader. Moving Mmap/Box does not move their allocations;
        // stream is destroyed before either owner, including on enumeration failure/disposal.
        let data: &'static [u8] = slice::from_raw_parts(mapping.as_ptr(), mapping.len());
        let summary_ref: &'static Option<mcap::Summary> = &*(&*summary as *const _);
        let start = v["start"].as_u64();
        let end = v["end"].as_u64();
        let topic = v["topic"].as_str().map(str::to_owned);
        let mut indexed = false;
        // Mixed files may contain messages outside chunks; only use chunk indexes when safe.
        let mut offset = 8usize;
        let mut top_level_message = false;
        while offset + 9 <= data.len() {
            let op = data[offset];
            if op == records::op::MESSAGE {
                top_level_message = true;
                break;
            }
            let len = u64::from_le_bytes(data[offset + 1..offset + 9].try_into().unwrap());
            let Some(next) = usize::try_from(len)
                .ok()
                .and_then(|n| offset.checked_add(9)?.checked_add(n))
            else {
                return Err("Invalid record length".into());
            };
            if next > data.len() {
                break;
            }
            offset = next;
            if op == records::op::FOOTER {
                break;
            }
        }
        let stream: RecordStream = if mode == "messages" {
            let messages: Box<dyn Iterator<Item = mcap::McapResult<mcap::Message<'static>>>> =
                if let Some(s) = summary_ref.as_ref().filter(|s| {
                    !top_level_message
                        && !s.chunk_indexes.is_empty()
                        && !s.channels.is_empty()
                        && s.stats
                            .as_ref()
                            .map(|x| x.chunk_count as usize == s.chunk_indexes.len())
                            .unwrap_or(false)
                }) {
                    indexed = true;
                    let indexes = s
                        .chunk_indexes
                        .iter()
                        .filter(|i| {
                            start.map(|x| i.message_end_time >= x).unwrap_or(true)
                                && end.map(|x| i.message_start_time < x).unwrap_or(true)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    Box::new(indexes.into_iter().flat_map(
                        move |i| -> Box<
                            dyn Iterator<Item = mcap::McapResult<mcap::Message<'static>>>,
                        > {
                            match s.stream_chunk(data, &i) {
                                Ok(iter) => Box::new(iter.map(|result| {
                                    result.map(|m| mcap::Message {
                                        channel: m.channel,
                                        sequence: m.sequence,
                                        log_time: m.log_time,
                                        publish_time: m.publish_time,
                                        data: Cow::Owned(m.data.into_owned()),
                                    })
                                })),
                                Err(e) => Box::new(std::iter::once(Err(e))),
                            }
                        },
                    ))
                } else {
                    Box::new(if recovery {
                        mcap::MessageStream::new_with_options(
                            data,
                            enumset::enum_set!(read::Options::IgnoreEndMagic),
                        )?
                    } else {
                        mcap::MessageStream::new(data)?
                    })
                };
            Box::new(messages.filter_map(move |result| {
                match result {
                    Ok(m)
                        if start.map(|x| m.log_time < x).unwrap_or(false)
                            || end.map(|x| m.log_time >= x).unwrap_or(false)
                            || topic
                                .as_ref()
                                .map(|t| *t != m.channel.topic)
                                .unwrap_or(false) =>
                    {
                        None
                    }
                    Ok(m) => Some(Ok(message_output(m))),
                    Err(e) => Some(Err(e.into())),
                }
            }))
        } else {
            let mode = mode.to_owned();
            let mut seen = std::collections::HashSet::<u16>::new();
            Box::new(read::ChunkFlattener::new(data)?.filter_map(move |result| match result {
                Ok(records::Record::Schema{header,data}) if mode=="schemas" && seen.insert(header.id)=>Some(Ok((json!({"id":header.id,"name":header.name,"encoding":header.encoding}),data.into_owned()))),
                Ok(records::Record::Channel(c)) if mode=="channels" && seen.insert(c.id)=>Some(Ok((json!({"id":c.id,"topic":c.topic,"messageEncoding":c.message_encoding,"schemaId":c.schema_id,"metadata":c.metadata}),vec![]))),
                Ok(records::Record::Metadata(m)) if mode=="metadata"=>Some(Ok((json!({"name":m.name,"values":m.metadata}),vec![]))),
                Ok(records::Record::Attachment{header,data,..}) if mode=="attachments"=>Some(Ok((json!({"name":header.name,"mediaType":header.media_type,"logTime":header.log_time,"createTime":header.create_time}),data.into_owned()))),
                Ok(_)=>None,Err(e)=>Some(Err(e.into()))
            }))
        };
        *handle = Box::into_raw(Box::new(Reader {
            stream,
            _summary: summary,
            _mapping: mapping,
            _file: file,
            failed: false,
        }));
        out.value = indexed as u64;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_next(handle: *mut Reader, out: *mut Response) -> i32 {
    let status = guard(out, |out| {
        let r = handle.as_mut().ok_or("Null reader")?;
        if r.failed {
            return Err("Reader has failed".into());
        }
        match r.stream.next() {
            Some(result) => {
                let (header, data) = result?;
                respond(out, serde_json::to_vec(&header)?, data, 0);
                Ok(0)
            }
            None => Ok(1),
        }
    });
    if status < 0 {
        if let Some(r) = handle.as_mut() {
            r.failed = true;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_free(handle: *mut Reader) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(handle))));
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_validate(p: *const u8, n: usize, out: *mut Response) -> i32 {
    guard(out, |out| {
        let v = request(p, n)?;
        let (file, mapping) = open_map(string(&v, "path")?)?;
        let mut cursor = std::io::Cursor::new(&mapping[..]);
        let mut reader = sans_io::LinearReader::new_with_options(
            sans_io::LinearReaderOptions::default()
                .with_validate_chunk_crcs(true)
                .with_validate_data_section_crc(true)
                .with_validate_summary_section_crc(true)
                .with_check_finishes_after_end_magic(true)
                .with_record_length_limit(mapping.len()),
        );
        let mut count = 0u64;
        while let Some(event) = reader.next_event() {
            match event? {
                sans_io::LinearReadEvent::ReadRequest(n) => {
                    let n = cursor.read(reader.insert(n))?;
                    reader.notify_read(n);
                }
                sans_io::LinearReadEvent::Record { opcode, data } => {
                    mcap::parse_record(opcode, data)?;
                    count += 1;
                }
            }
        }
        drop(file);
        out.value = count;
        Ok(0)
    })
}
