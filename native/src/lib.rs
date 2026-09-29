//! Private C ABI. No borrowed native memory crosses the public managed API.
mod buffer_reader;
#[cfg(test)]
mod coverage_tests;
mod errors;
mod extended;
mod io;
mod memory;
mod sort_arena;
use io::{Callbacks, Input, Output};
use mcap::{records, sans_io};
use memmap2::Mmap;
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
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
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct MessageHeader {
    channel_id: u16,
    reserved: u16,
    sequence: u32,
    log_time: u64,
    publish_time: u64,
}
fn buffer(v: Vec<u8>) -> (*mut u8, usize) {
    if v.is_empty() {
        return (ptr::null_mut(), 0);
    }
    let b = v.into_boxed_slice();
    let n = b.len();
    (Box::into_raw(b) as *mut u8, n)
}
fn respond(out: &mut Response, h: Vec<u8>, d: Vec<u8>, v: u64) {
    (out.json, out.json_len) = buffer(h);
    (out.data, out.data_len) = buffer(d);
    out.value = v;
}
fn guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    if out.is_null() {
        return -1;
    }
    let out = unsafe { &mut *out };
    *out = Response::default();
    match catch_unwind(AssertUnwindSafe(|| f(out))) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            respond(out, errors::encode(e.as_ref()), vec![], 0);
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
fn string<'a>(v: &'a Value, k: &str) -> Outcome<&'a str> {
    v[k].as_str()
        .ok_or_else(|| format!("Missing string: {k}").into())
}
fn number(v: &Value, k: &str) -> Outcome<u64> {
    v[k].as_u64()
        .ok_or_else(|| format!("Missing integer: {k}").into())
}
fn map(v: &Value) -> Outcome<BTreeMap<String, String>> {
    Ok(serde_json::from_value(v.clone())?)
}
#[no_mangle]
pub extern "C" fn fm_abi_version() -> u32 {
    6
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(p, n)))
    }
}

pub struct Writer {
    inner: Option<mcap::Writer<Output>>,
    failed: bool,
    recoverable_errors: u32,
    attachment: Option<u64>,
    summary: Option<Value>,
    native_summary: Option<std::sync::Arc<mcap::Summary>>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(w) = self.inner.take() {
            drop(w.into_inner());
        }
    }
}
fn options(v: &Value, seekable: bool) -> Outcome<mcap::WriteOptions> {
    let compression = match string(v, "compression")? {
        "none" => None,
        "lz4" => Some(mcap::Compression::Lz4),
        "zstd" => Some(mcap::Compression::Zstd),
        _ => return Err("Unknown compression".into()),
    };
    let mut o = mcap::WriteOptions::new()
        .compression(compression)
        .chunk_size(v["chunkSize"].as_u64())
        .use_chunks(v["useChunks"].as_bool().unwrap_or(true))
        .profile(string(v, "profile")?)
        .library(v["library"].as_str().unwrap_or(mcap::LIBRARY_IDENTIFIER))
        .disable_seeking(v["disableSeeking"].as_bool().unwrap_or(!seekable));
    if !seekable && v["disableSeeking"].as_bool() == Some(false) {
        return Err("Non-seekable output requires DisableSeeking".into());
    }
    macro_rules! flag {
        ($key:literal,$method:ident) => {
            if let Some(x) = v[$key].as_bool() {
                o = o.$method(x);
            }
        };
    }
    flag!("emitSummaryRecords", emit_summary_records);
    flag!("emitSummaryOffsets", emit_summary_offsets);
    flag!("emitStatistics", emit_statistics);
    flag!("emitMessageIndexes", emit_message_indexes);
    flag!("emitChunkIndexes", emit_chunk_indexes);
    flag!("emitAttachmentIndexes", emit_attachment_indexes);
    flag!("emitMetadataIndexes", emit_metadata_indexes);
    flag!("repeatChannels", repeat_channels);
    flag!("repeatSchemas", repeat_schemas);
    flag!("calculateChunkCrcs", calculate_chunk_crcs);
    flag!("calculateDataSectionCrc", calculate_data_section_crc);
    flag!("calculateSummarySectionCrc", calculate_summary_section_crc);
    flag!("calculateAttachmentCrcs", calculate_attachment_crcs);
    if let Some(x) = v["compressionLevel"].as_u64() {
        o = o.compression_level(x.try_into()?);
    }
    if let Some(x) = v["compressionThreads"].as_u64() {
        o = o.compression_threads(x.try_into()?);
    }
    Ok(o)
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_open(
    p: *const u8,
    n: usize,
    callbacks: *const Callbacks,
    handle: *mut *mut Writer,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let recoverable_errors = match v["options"].get("recoverableErrors") {
            None => 31,
            Some(value) => value
                .as_u64()
                .filter(|n| n & !31 == 0)
                .ok_or("Invalid recovery flags")? as u32,
        };
        let output = if let Some(c) = callbacks.as_ref() {
            Output::Stream(*c)
        } else {
            Output::File(
                File::options()
                    .write(true)
                    .read(true)
                    .create_new(true)
                    .open(string(&v, "path")?)?,
            )
        };
        let seekable = callbacks.as_ref().map(|c| c.seekable != 0).unwrap_or(true);
        *handle = Box::into_raw(Box::new(Writer {
            inner: Some(options(&v["options"], seekable)?.create(output)?),
            failed: false,
            recoverable_errors,
            attachment: None,
            summary: None,
            native_summary: None,
        }));
        Ok(0)
    })
}
// Only audited, pre-mutation return sites may construct this marker.
#[derive(Debug)]
struct SafeRejection(mcap::McapError);
impl std::fmt::Display for SafeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for SafeRejection {}
fn registration_result<T>(
    result: mcap::McapResult<T>,
    mask: u32,
    schema: bool,
    explicit: bool,
) -> Outcome<T> {
    result.map_err(|e| {
        let bit = match (&e, schema, explicit) {
            (mcap::McapError::InvalidSchemaId, true, true) => 1,
            (mcap::McapError::ConflictingSchemas(_), true, true) => 2,
            (mcap::McapError::UnknownSchema(..), false, _) => 4,
            (mcap::McapError::ConflictingChannels(_), false, true) => 8,
            _ => 0,
        };
        if mask & bit != 0 {
            Box::new(SafeRejection(e)) as Error
        } else {
            Box::new(e) as Error
        }
    })
}
fn writer_guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    guard(out, |out| match f(out) {
        Err(e) if e.is::<SafeRejection>() => {
            let e = e.downcast::<SafeRejection>().unwrap();
            respond(out, errors::encode(&e.0), vec![], 0);
            Ok(-2)
        }
        result => result,
    })
}
fn writer_result(handle: *mut Writer, status: i32) -> i32 {
    if status < 0 && status != -2 {
        if let Some(w) = unsafe { handle.as_mut() } {
            w.failed = true;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_message(
    handle: *mut Writer,
    h: *const MessageHeader,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let s = writer_guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let h = h.as_ref().ok_or("Null header")?;
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write_to_known_channel(
                &records::MessageHeader {
                    channel_id: h.channel_id,
                    sequence: h.sequence,
                    log_time: h.log_time,
                    publish_time: h.publish_time,
                },
                bytes(p, n)?,
            )
            .map_err(|e| {
                if w.recoverable_errors & 16 != 0
                    && matches!(e, mcap::McapError::UnknownChannel(..))
                {
                    Box::new(SafeRejection(e)) as Error
                } else {
                    Box::new(e) as Error
                }
            })?;
        Ok(0)
    });
    writer_result(handle, s)
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
    let status = writer_guard(out, |out| {
        let v = if n == 0 { Value::Null } else { request(p, n)? };
        writer_control(handle, op, &v, bytes(data, len)?, out)
    });
    writer_result(handle, status)
}
unsafe fn writer_control(
    handle: *mut Writer,
    op: u32,
    v: &Value,
    payload: &[u8],
    out: &mut Response,
) -> Outcome<i32> {
    let holder = handle.as_mut().ok_or("Null writer")?;
    if holder.failed {
        return Err("Writer failed".into());
    }
    if op == 12 {
        if let Some(s) = &holder.summary {
            respond(out, serde_json::to_vec(s)?, vec![], 0);
            return Ok(0);
        }
        return Err("Complete must succeed first".into());
    }
    if holder.attachment.is_some() && op != 9 && op != 10 {
        return Err("Attachment is in progress".into());
    }
    let len = payload.len();
    let w = holder.inner.as_mut().ok_or("Writer completed")?;
    out.value = match op {
        1 => {
            ({
                if let Some(id) = v["id"].as_u64() {
                    registration_result(
                        w.add_schema_with_id(
                            id.try_into()?,
                            string(v, "name")?,
                            string(v, "encoding")?,
                            payload,
                        ),
                        holder.recoverable_errors,
                        true,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_schema(string(v, "name")?, string(v, "encoding")?, payload),
                        holder.recoverable_errors,
                        true,
                        false,
                    )?
                }
            }) as u64
        }
        2 => {
            ({
                if let Some(id) = v["id"].as_u64() {
                    registration_result(
                        w.add_channel_with_id(
                            id.try_into()?,
                            number(v, "schema_id")?.try_into()?,
                            string(v, "topic")?,
                            string(v, "encoding")?,
                            &map(&v["metadata"])?,
                        ),
                        holder.recoverable_errors,
                        false,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_channel(
                            number(v, "schema_id")?.try_into()?,
                            string(v, "topic")?,
                            string(v, "encoding")?,
                            &map(&v["metadata"])?,
                        ),
                        holder.recoverable_errors,
                        false,
                        false,
                    )?
                }
            }) as u64
        }
        4 => {
            w.write_metadata(&records::Metadata {
                name: string(v, "name")?.into(),
                metadata: map(&v["metadata"])?,
            })?;
            0
        }
        5 => {
            w.attach(&mcap::Attachment {
                name: string(v, "name")?.into(),
                media_type: string(v, "media_type")?.into(),
                log_time: number(v, "log_time")?,
                create_time: number(v, "create_time")?,
                data: Cow::Borrowed(payload),
            })?;
            0
        }
        6 => {
            w.flush()?;
            0
        }
        7 => {
            let s = w.finish()?;
            holder.summary = Some(summary_json(&s));
            holder.native_summary = Some(std::sync::Arc::new(s));
            let mut output = holder.inner.take().unwrap().into_inner();
            output.complete()?;
            0
        }
        8 => {
            let size = number(v, "length")?;
            w.start_attachment(
                size,
                records::AttachmentHeader {
                    name: string(v, "name")?.into(),
                    media_type: string(v, "media_type")?.into(),
                    log_time: number(v, "log_time")?,
                    create_time: number(v, "create_time")?,
                },
            )?;
            holder.attachment = Some(size);
            0
        }
        9 => {
            let left = holder.attachment.as_mut().ok_or("No attachment")?;
            *left = left
                .checked_sub(len as u64)
                .ok_or("Attachment length exceeded")?;
            w.put_attachment_bytes(payload)?;
            0
        }
        10 => {
            if holder.attachment != Some(0) {
                return Err("Attachment length mismatch".into());
            }
            w.finish_attachment()?;
            holder.attachment = None;
            0
        }
        11 => {
            let opts = if v["includeInChunks"].as_bool().unwrap_or(false) {
                enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
            } else {
                enumset::EnumSet::new()
            };
            w.write_private_record(number(v, "opcode")?.try_into()?, payload, opts)?;
            0
        }
        _ => return Err("Unknown writer operation".into()),
    };
    Ok(0)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_free(p: *mut Writer) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}

fn open_input(path: &str) -> Outcome<Input> {
    let mut o = File::options();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(1);
    }
    let f = o.open(path)?;
    let mapping = unsafe { Mmap::map(&f)? };
    Ok(Input::Map {
        mapping,
        _file: f,
        position: 0,
    })
}
fn chunk_json(c: &records::ChunkIndex) -> Value {
    json!({"messageStartTime":c.message_start_time,"messageEndTime":c.message_end_time,"chunkStartOffset":c.chunk_start_offset,"chunkLength":c.chunk_length,"messageIndexOffsets":c.message_index_offsets,"messageIndexLength":c.message_index_length,"compression":c.compression,"compressedSize":c.compressed_size,"uncompressedSize":c.uncompressed_size})
}
fn stats_json(s: &records::Statistics) -> Value {
    json!({"messageCount":s.message_count,"schemaCount":s.schema_count,"channelCount":s.channel_count,"attachmentCount":s.attachment_count,"metadataCount":s.metadata_count,"chunkCount":s.chunk_count,"messageStartTime":s.message_start_time,"messageEndTime":s.message_end_time,"channelMessageCounts":s.channel_message_counts})
}
fn attachment_index_json(a: &records::AttachmentIndex) -> Value {
    json!({"offset":a.offset,"length":a.length,"logTime":a.log_time,"createTime":a.create_time,"dataSize":a.data_size,"name":a.name,"mediaType":a.media_type})
}
fn metadata_index_json(a: &records::MetadataIndex) -> Value {
    json!({"offset":a.offset,"length":a.length,"name":a.name})
}
fn summary_json(s: &mcap::Summary) -> Value {
    json!({"statistics":s.stats.as_ref().map(stats_json),"chunkIndexes":s.chunk_indexes.iter().map(chunk_json).collect::<Vec<_>>(),"attachmentIndexes":s.attachment_indexes.iter().map(attachment_index_json).collect::<Vec<_>>(),"metadataIndexes":s.metadata_indexes.iter().map(metadata_index_json).collect::<Vec<_>>(),"schemaIds":s.schemas.keys().collect::<Vec<_>>(),"channelIds":s.channels.keys().collect::<Vec<_>>()})
}
fn empty_summary() -> Value {
    json!({"statistics":null,"chunkIndexes":[],"attachmentIndexes":[],"metadataIndexes":[],"schemaIds":[],"channelIds":[]})
}

pub struct Reader {
    delivery: memory::Delivery,
    scratch: memory::Delivery,
    memory_peak: u64,
    indexed: Option<sans_io::IndexedReader>,
    sorted: bool,
    order: u64,
    topics: Option<std::collections::BTreeSet<String>>,
    limit: Option<usize>,
    arena: sort_arena::Arena,
    indexed_summary: Option<Value>,
    parser: Option<sans_io::LinearReader>,
    input: Input,

    schemas: BTreeMap<u16, (records::SchemaHeader, Vec<u8>)>,
    channels: BTreeMap<u16, records::Channel>,
    summary: Value,
    summary_present: bool,
    in_summary: bool,
    ended: bool,
    failed: bool,
    count: u64,
    topic: Option<String>,
    start: Option<u64>,
    end: Option<u64>,
    messages: bool,
}
fn parser(top: bool, limit: Option<usize>) -> sans_io::LinearReader {
    let mut o = sans_io::LinearReaderOptions::default()
        .with_emit_chunks(top)
        .with_prevalidate_chunk_crcs(!top)
        .with_validate_data_section_crc(true)
        .with_validate_summary_section_crc(true)
        .with_check_finishes_after_end_magic(true);
    if let Some(n) = limit {
        o = o.with_record_length_limit(n);
    }
    sans_io::LinearReader::new_with_options(o)
}
impl Reader {
    fn observe(&mut self, op: u8, data: &[u8]) -> Outcome<()> {
        self.count += 1;
        match mcap::parse_record(op, data)? {
            records::Record::Schema { header, data } => {
                if header.id == 0 {
                    return Err("Invalid schema ID".into());
                }
                if let Some((old, bytes)) = self.schemas.get(&header.id) {
                    if old != &header || bytes.as_slice() != data.as_ref() {
                        return Err("Conflicting schema".into());
                    }
                }
                if self.in_summary {
                    self.summary["schemaIds"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(header.id));
                }
                if !self.schemas.contains_key(&header.id) {
                    self.schemas.insert(header.id, (header, data.into_owned()));
                }
            }
            records::Record::Channel(c) => {
                if self.messages && c.schema_id != 0 && !self.schemas.contains_key(&c.schema_id) {
                    return Err("Unknown schema".into());
                }
                if let Some(old) = self.channels.get(&c.id) {
                    if old != &c {
                        return Err("Conflicting channel".into());
                    }
                }
                if self.in_summary {
                    self.summary["channelIds"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(c.id));
                }
                self.channels.entry(c.id).or_insert(c);
            }
            records::Record::DataEnd(_) => self.in_summary = true,
            records::Record::Footer(f) => self.summary_present = f.summary_start != 0,
            records::Record::Statistics(s) if self.in_summary => {
                self.summary["statistics"] = stats_json(&s)
            }
            records::Record::ChunkIndex(c) if self.in_summary => self.summary["chunkIndexes"]
                .as_array_mut()
                .unwrap()
                .push(chunk_json(&c)),
            records::Record::AttachmentIndex(a) if self.in_summary => self.summary
                ["attachmentIndexes"]
                .as_array_mut()
                .unwrap()
                .push(attachment_index_json(&a)),
            records::Record::MetadataIndex(a) if self.in_summary => self.summary["metadataIndexes"]
                .as_array_mut()
                .unwrap()
                .push(metadata_index_json(&a)),
            _ => {}
        }
        Ok(())
    }
    fn select(&self, h: &records::MessageHeader) -> Outcome<bool> {
        let c = self
            .channels
            .get(&h.channel_id)
            .ok_or("Unknown message channel")?;
        Ok(!self.start.is_some_and(|n| h.log_time < n)
            && !self.end.is_some_and(|n| h.log_time >= n)
            && !self.topic.as_ref().is_some_and(|t| t != &c.topic)
            && !self.topics.as_ref().is_some_and(|t| !t.contains(&c.topic)))
    }
    fn next_with(
        &mut self,
        mut sink: impl FnMut(&mut Self, u8, &[u8], MessageHeader) -> Outcome<i32>,
    ) -> Outcome<i32> {
        if self.ended {
            return Ok(1);
        }
        if let Some(mut indexed) = self.indexed.take() {
            let result = (|| loop {
                match indexed.next_event().transpose()? {
                    None => {
                        self.ended = true;
                        return Ok(1);
                    }
                    Some(sans_io::IndexedReadEvent::ReadChunkRequest { offset, length }) => {
                        if let Input::Map {
                            mapping, position, ..
                        } = &mut self.input
                        {
                            let start = usize::try_from(offset)?;
                            let end = start.checked_add(length).ok_or(mcap::McapError::BadIndex)?;
                            let data = mapping.get(start..end).ok_or(mcap::McapError::BadIndex)?;
                            indexed.insert_chunk_record_data(offset, data)?;
                            *position = end;
                        } else {
                            self.input.seek(SeekFrom::Start(offset))?;
                            self.scratch.data.clear();
                            self.scratch.reserve(length)?;
                            self.scratch.data.resize(length, 0);
                            self.update_memory_peak();
                            self.input.read_exact(&mut self.scratch.data)?;
                            indexed.insert_chunk_record_data(offset, &self.scratch.data)?;
                            self.scratch.stats.copied += length as u64;
                            self.scratch.release();
                        }
                    }
                    Some(sans_io::IndexedReadEvent::Message { header, data }) => {
                        self.count += 1;
                        if !self.select(&header)? {
                            continue;
                        }
                        let h = MessageHeader {
                            channel_id: header.channel_id,
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            reserved: 0,
                        };
                        return sink(self, records::op::MESSAGE, data, h);
                    }
                }
            })();
            self.indexed = if self.ended { None } else { Some(indexed) };
            return result;
        }
        let mut parser = self.parser.take().ok_or("Missing parser")?;
        let result = (|| loop {
            match parser.next_event().transpose()? {
                None => {
                    self.ended = true;
                    return Ok(1);
                }
                Some(sans_io::LinearReadEvent::ReadRequest(n)) => {
                    let n = self.input.read(parser.insert(n.min(65536)))?;
                    self.delivery.stats.copied += n as u64;
                    parser.notify_read(n);
                }
                Some(sans_io::LinearReadEvent::Record { opcode, data }) => {
                    self.observe(opcode, data)?;
                    if self.messages {
                        if opcode != records::op::MESSAGE {
                            continue;
                        }
                        let records::Record::Message { header, data } =
                            mcap::parse_record(opcode, data)?
                        else {
                            unreachable!()
                        };
                        if !self.select(&header)? {
                            continue;
                        }
                        let h = MessageHeader {
                            channel_id: header.channel_id,
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            reserved: 0,
                        };
                        return sink(self, opcode, &data, h);
                    }
                    return sink(self, opcode, data, MessageHeader::default());
                }
            }
        })();
        self.parser = if self.ended { None } else { Some(parser) };
        result
    }
    fn update_memory_peak(&mut self) {
        self.memory_peak = self.memory_peak.max(
            self.delivery.stats.current + self.scratch.stats.current + self.arena.stats.current,
        );
    }
    unsafe fn read(&mut self, dest: *mut u8, capacity: usize, out: &mut Response) -> Outcome<i32> {
        if self.sorted {
            let status = self
                .arena
                .read(dest, capacity, &mut self.delivery.header, out)?;
            self.delivery.opcode = records::op::MESSAGE;
            if status == 1 {
                self.ended = true;
            }
            return Ok(status);
        }
        if self.delivery.active {
            out.value = self.delivery.data.len() as u64;
            return self.delivery.retry(dest, capacity);
        }
        self.next_with(|r, op, data, h| {
            r.delivery.opcode = op;
            r.delivery.header = h;
            out.value = data.len() as u64;
            r.delivery.deliver(data, dest, capacity)
        })
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_open(
    p: *const u8,
    n: usize,
    callbacks: *const Callbacks,
    handle: *mut *mut Reader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let input = if let Some(c) = callbacks.as_ref() {
            Input::Stream(*c)
        } else {
            open_input(string(&v, "path")?)?
        };
        let limit = v["recordLengthLimit"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
        let mut reader = Reader {
            delivery: memory::Delivery {
                options: memory::Options::parse(&v["options"]["Memory"])?,
                ..Default::default()
            },
            memory_peak: 0,
            scratch: memory::Delivery {
                options: memory::Options {
                    retained: memory::Options::parse(&v["options"]["Memory"])?.retained,
                    ..Default::default()
                },
                ..Default::default()
            },
            indexed: None,
            sorted: false,
            order: v["order"].as_u64().unwrap_or(2),
            topics: v["topics"].as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            }),
            limit,
            arena: sort_arena::Arena::default(),
            indexed_summary: None,
            parser: Some(sans_io::LinearReader::new_with_options(
                extended::linear_options(&v["options"])?
                    .with_emit_chunks(v["topLevel"].as_bool().unwrap_or(false)),
            )),
            input,
            schemas: BTreeMap::new(),
            channels: BTreeMap::new(),
            summary: empty_summary(),
            summary_present: false,
            in_summary: false,
            ended: false,
            failed: false,
            count: 0,
            topic: v["topic"].as_str().map(str::to_owned),
            start: v["start"].as_u64(),
            end: v["end"].as_u64(),
            messages: v["messages"].as_bool().unwrap_or(true),
        };
        let linear_settings = v["options"]
            .as_object()
            .is_some_and(|o| o.iter().any(|(_, v)| v.as_bool() == Some(true)));
        if v["indexedOnly"].as_bool().unwrap_or(false) && linear_settings {
            return Err("Linear parser options cannot be applied to indexed reading".into());
        }
        if reader.messages
            && !linear_settings
            && (reader.order != 2
                || reader.start.is_some()
                || reader.end.is_some()
                || reader.topic.is_some()
                || reader.topics.is_some())
            && reader.input.seekable()
        {
            reader.try_indexed()?;
        }
        if v["indexedOnly"].as_bool().unwrap_or(false) && reader.indexed.is_none() {
            return Err("Indexed reading requires a complete indexed summary".into());
        }
        if reader.messages && reader.order != 2 && reader.indexed.is_none() {
            if v["allowBufferedSort"].as_bool() == Some(false) {
                return Ok(3);
            }
            while reader.next_with(|r, _op, data, h| {
                r.arena.push(h, data, r.delivery.options.sort)?;
                r.update_memory_peak();
                Ok(0)
            })? != 1
            {}
            reader.arena.sort(reader.order == 1);
            reader.sorted = true;
            reader.ended = false;
        }
        *handle = Box::into_raw(Box::new(reader));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_next(
    handle: *mut Reader,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    opcode: *mut u8,
    out: *mut Response,
) -> i32 {
    let s = guard(out, |out| {
        if !header.is_null() {
            *header = MessageHeader::default();
        }
        if !opcode.is_null() {
            *opcode = 0;
        }
        let r = handle.as_mut().ok_or("Null reader")?;
        if r.failed {
            return Err("Reader failed".into());
        }
        let result = r.read(dest, capacity, out);
        r.update_memory_peak();
        let status = result?;
        if status == 1 {
            r.delivery.discard();
            r.scratch.discard();
            out.value = r.count;
            if !header.is_null() {
                (*header).reserved = if r.indexed_summary.is_some() || r.sorted {
                    1
                } else {
                    0
                };
            }
        } else {
            if !opcode.is_null() {
                *opcode = r.delivery.opcode;
            }
            if !header.is_null() {
                *header = r.delivery.header;
            }
        }
        Ok(status)
    });
    if s < 0 {
        if let Some(r) = handle.as_mut() {
            r.failed = true;
            r.delivery.discard();
            r.scratch.discard();
            r.parser = None;
            r.indexed = None;
            r.arena.clear();
        }
    }
    s
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_describe(
    handle: *mut Reader,
    kind: u32,
    id: u16,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let r = handle.as_mut().ok_or("Null reader")?;
        match kind {
            1 => {
                let (h, d) = r.schemas.get(&id).ok_or("Unknown schema")?;
                respond(
                    out,
                    serde_json::to_vec(&json!({"id":h.id,"name":h.name,"encoding":h.encoding}))?,
                    d.clone(),
                    0,
                );
            }
            2 => {
                let c = r.channels.get(&id).ok_or("Unknown channel")?;
                respond(
                    out,
                    serde_json::to_vec(
                        &json!({"id":c.id,"schemaId":c.schema_id,"topic":c.topic,"messageEncoding":c.message_encoding,"metadata":c.metadata}),
                    )?,
                    vec![],
                    0,
                );
            }
            _ => return Err("Unknown description".into()),
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_free(p: *mut Reader) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}

impl Reader {
    fn read_summary(&mut self) -> Outcome<Option<mcap::Summary>> {
        if !self.input.seekable() {
            return Err("Stream is not seekable".into());
        }
        let pos = self.input.stream_position()?;
        let result = (|| {
            let mut opts = sans_io::SummaryReaderOptions::default();
            if let Some(n) = self.limit {
                opts = opts.with_record_length_limit(n);
            }
            let mut s = sans_io::SummaryReader::new_with_options(opts);
            while let Some(e) = s.next_event() {
                match e? {
                    sans_io::SummaryReadEvent::ReadRequest(n) => {
                        let n = self.input.read(s.insert(n.min(65536)))?;
                        s.notify_read(n);
                    }
                    sans_io::SummaryReadEvent::SeekRequest(p) => {
                        let n = self.input.seek(p)?;
                        s.notify_seeked(n);
                    }
                }
            }
            Ok(s.finish())
        })();
        self.input.seek(SeekFrom::Start(pos))?;
        result
    }
    fn record_at(&mut self, offset: u64) -> Outcome<(u8, Vec<u8>)> {
        if !self.input.seekable() {
            return Err("Stream is not seekable".into());
        }
        let pos = self.input.stream_position()?;
        let result = (|| {
            self.input.seek(SeekFrom::Start(offset))?;
            let mut h = [0u8; 9];
            self.input.read_exact(&mut h)?;
            let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
            if self.limit.is_some_and(|limit| n > limit) {
                return Err(mcap::McapError::RecordTooLarge {
                    opcode: h[0],
                    len: n as u64,
                }
                .into());
            }
            let body_start = self.input.stream_position()?;
            let end = self.input.seek(SeekFrom::End(0))?;
            if n as u64 > end.saturating_sub(body_start) {
                return Err("Record exceeds source length".into());
            }
            self.input.seek(SeekFrom::Start(body_start))?;
            let mut data = Vec::new();
            data.try_reserve_exact(n)?;
            data.resize(n, 0);
            self.input.read_exact(&mut data)?;
            mcap::parse_record(h[0], &data)?;
            Ok((h[0], data))
        })();
        self.input.seek(SeekFrom::Start(pos))?;
        result
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_summary(handle: *mut Reader, out: *mut Response) -> i32 {
    guard(out, |out| {
        let r = handle.as_mut().ok_or("Null reader")?;
        if let Some(s) = &r.indexed_summary {
            respond(out, serde_json::to_vec(s)?, vec![], 0);
            return Ok(0);
        }
        if r.ended {
            if r.summary_present {
                respond(out, serde_json::to_vec(&r.summary)?, vec![], 0);
            }
            return Ok(0);
        }
        let summary = match r.read_summary() {
            Ok(s) => s,
            Err(e)
                if e.downcast_ref::<mcap::McapError>()
                    .is_some_and(|e| matches!(e, mcap::McapError::UnknownSchema(..))) =>
            {
                if let Some(s) = r.scan_summary()? {
                    respond(out, serde_json::to_vec(&s)?, vec![], 0);
                }
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        if let Some(s) = summary {
            for (id, c) in &s.channels {
                r.channels.entry(*id).or_insert(records::Channel {
                    id: *id,
                    schema_id: c.schema.as_ref().map(|s| s.id).unwrap_or(0),
                    topic: c.topic.clone(),
                    message_encoding: c.message_encoding.clone(),
                    metadata: c.metadata.clone(),
                });
            }
            for (id, schema) in &s.schemas {
                r.schemas.entry(*id).or_insert((
                    records::SchemaHeader {
                        id: *id,
                        name: schema.name.clone(),
                        encoding: schema.encoding.clone(),
                    },
                    schema.data.to_vec(),
                ));
            }
            respond(out, serde_json::to_vec(&summary_json(&s))?, vec![], 0);
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_record_at(
    handle: *mut Reader,
    offset: u64,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let r = handle.as_mut().ok_or("Null reader")?;
        let (op, data) = r.record_at(offset)?;
        respond(out, vec![], data, op as u64);
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_validate(p: *const u8, n: usize, out: *mut Response) -> i32 {
    guard(out, |out| {
        let v = request(p, n)?;
        let mut input = open_input(string(&v, "path")?)?;
        let limit = v["recordLengthLimit"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
        let mut parser = parser(false, limit);
        let mut count = 0;
        while let Some(e) = parser.next_event() {
            match e? {
                sans_io::LinearReadEvent::ReadRequest(n) => {
                    let n = input.read(parser.insert(n.min(65536)))?;
                    parser.notify_read(n);
                }
                sans_io::LinearReadEvent::Record { opcode, data } => {
                    mcap::parse_record(opcode, data)?;
                    count += 1;
                }
            }
        }
        out.value = count;
        Ok(0)
    })
}

impl Reader {
    fn try_indexed(&mut self) -> Outcome<()> {
        let summary = match self.read_summary() {
            Ok(s) => s,
            Err(e)
                if e.downcast_ref::<mcap::McapError>()
                    .is_some_and(|e| matches!(e, mcap::McapError::UnknownSchema(..))) =>
            {
                return Ok(())
            }
            Err(e) => return Err(e),
        };
        let Some(summary) = summary else {
            return Ok(());
        };
        if summary.chunk_indexes.is_empty()
            || summary.channels.is_empty()
            || summary.stats.as_ref().map(|s| s.chunk_count as usize)
                != Some(summary.chunk_indexes.len())
        {
            return Ok(());
        }
        let position = self.input.stream_position()?;
        let safe = (|| -> Outcome<bool> {
            self.input.seek(SeekFrom::Start(0))?;
            let mut magic = [0; 8];
            self.input.read_exact(&mut magic)?;
            if &magic != mcap::MAGIC {
                return Err("Bad start magic".into());
            }
            let mut chunks = std::collections::BTreeMap::new();
            loop {
                let record_offset = self.input.stream_position()?;
                let mut h = [0; 9];
                self.input.read_exact(&mut h)?;
                if h[0] == records::op::MESSAGE {
                    return Ok(false);
                }
                if h[0] == records::op::CHUNK {
                    let length = u64::from_le_bytes(h[1..].try_into()?);
                    chunks.insert(
                        record_offset,
                        length.checked_add(9).ok_or(mcap::McapError::BadIndex)?,
                    );
                }
                if h[0] == records::op::FOOTER {
                    if chunks.len() != summary.chunk_indexes.len() {
                        return Ok(false);
                    }
                    for index in &summary.chunk_indexes {
                        if chunks.remove(&index.chunk_start_offset) != Some(index.chunk_length) {
                            return Err(mcap::McapError::BadIndex.into());
                        }
                    }
                    return Ok(chunks.is_empty());
                }
                let n = u64::from_le_bytes(h[1..].try_into()?);
                self.input.seek(SeekFrom::Current(i64::try_from(n)?))?;
            }
        })();
        self.input.seek(SeekFrom::Start(position))?;
        if !safe? {
            return Ok(());
        }
        for (id, s) in &summary.schemas {
            self.schemas.insert(
                *id,
                (
                    records::SchemaHeader {
                        id: *id,
                        name: s.name.clone(),
                        encoding: s.encoding.clone(),
                    },
                    s.data.to_vec(),
                ),
            );
        }
        for (id, c) in &summary.channels {
            self.channels.insert(
                *id,
                records::Channel {
                    id: *id,
                    schema_id: c.schema.as_ref().map(|s| s.id).unwrap_or(0),
                    topic: c.topic.clone(),
                    message_encoding: c.message_encoding.clone(),
                    metadata: c.metadata.clone(),
                },
            );
        }
        self.indexed_summary = Some(summary_json(&summary));
        let mut opts = sans_io::IndexedReaderOptions::default();
        opts.start = self.start;
        opts.end = self.end;
        opts.order = match self.order {
            0 => sans_io::indexed_reader::ReadOrder::LogTime,
            1 => sans_io::indexed_reader::ReadOrder::ReverseLogTime,
            _ => sans_io::indexed_reader::ReadOrder::File,
        };
        opts.include_topics = self.topics.clone().or_else(|| {
            self.topic
                .as_ref()
                .map(|t| [t.clone()].into_iter().collect())
        });
        opts.record_length_limit = self.limit;
        self.indexed = Some(sans_io::IndexedReader::new_with_options(&summary, opts)?);
        Ok(())
    }
}

// These are private C ABI layouts on all supported 64-bit targets, independent of Rust record layouts.
const _: () = assert!(std::mem::size_of::<MessageHeader>() == 24);
const _: () = assert!(std::mem::offset_of!(MessageHeader, log_time) == 8);
const _: () = assert!(std::mem::size_of::<Response>() == 40);
const _: () = assert!(std::mem::size_of::<Callbacks>() == 48);

impl Reader {
    // A valid file may omit repeated schemas but retain repeated channels. Upstream's
    // SummaryReader cannot resolve those alone; scan declarations without moving our cursor.
    fn scan_summary(&mut self) -> Outcome<Option<Value>> {
        let pos = self.input.stream_position()?;
        let old_summary = std::mem::replace(&mut self.summary, empty_summary());
        let (old_present, old_in, old_count) = (self.summary_present, self.in_summary, self.count);
        self.summary_present = false;
        self.in_summary = false;
        let result = (|| -> Outcome<Option<Value>> {
            self.input.seek(SeekFrom::Start(0))?;
            let mut p = parser(false, None);
            while let Some(e) = p.next_event() {
                match e? {
                    sans_io::LinearReadEvent::ReadRequest(n) => {
                        let n = self.input.read(p.insert(n.min(65536)))?;
                        p.notify_read(n);
                    }
                    sans_io::LinearReadEvent::Record { opcode, data } => {
                        self.observe(opcode, data)?
                    }
                }
            }
            Ok(if self.summary_present {
                Some(self.summary.clone())
            } else {
                None
            })
        })();
        self.summary = old_summary;
        self.summary_present = old_present;
        self.in_summary = old_in;
        self.count = old_count;
        self.input.seek(SeekFrom::Start(pos))?;
        result
    }
}

#[cfg(test)]
mod memory_probe;
