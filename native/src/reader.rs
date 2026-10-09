//! Stream/file read sessions, query selection and cursor-preserving random access.
// Internal classification sentinel; never returned through the C ABI.
const SKIP_RECORD: i32 = 3;
use super::{guard, request, respond, restored, string, MessageHeader, Outcome, Response};
use super::{engine, io, memory, sort_arena};
use super::io::{open_input, Callbacks, Input};
use super::summary::{
    attachment_index_json, chunk_json, empty_summary, metadata_index_json, stats_json,
    summary_json,
};
use mcap::{records, sans_io};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::{Read, Seek, SeekFrom}, ptr};

/// Declaration validity belongs to the expanded scan, independently of delivery mode.
#[derive(Default)]
struct Declarations {
    schemas: BTreeMap<u16, (records::SchemaHeader, Vec<u8>)>,
    channels: BTreeMap<u16, records::Channel>,
    scanned_schemas: std::collections::BTreeSet<u16>,
    scanned_channels: std::collections::BTreeSet<u16>,
}
impl Declarations {
    fn observe(&mut self, record: &records::Record<'_>, expanded: bool) -> Outcome<()> {
        match record {
            records::Record::Schema { header, data } => {
                if header.id == 0 { return Err(mcap::McapError::InvalidSchemaId.into()); }
                self.scanned_schemas.insert(header.id);
                if let Some((old, bytes)) = self.schemas.get(&header.id) {
                    if old != header || bytes.as_slice() != data.as_ref() {
                        return Err(mcap::McapError::ConflictingSchemas(header.name.clone()).into());
                    }
                } else {
                    self.schemas.insert(header.id, (header.clone(), data.to_vec()));
                }
            }
            records::Record::Channel(c) => {
                if expanded && c.schema_id != 0 && !self.scanned_schemas.contains(&c.schema_id) {
                    return Err(mcap::McapError::UnknownSchema(c.topic.clone(), c.schema_id).into());
                }
                self.scanned_channels.insert(c.id);
                if let Some(old) = self.channels.get(&c.id) {
                    if old != c { return Err(mcap::McapError::ConflictingChannels(c.topic.clone()).into()); }
                } else {
                    self.channels.insert(c.id, c.clone());
                }
            }
            records::Record::Message { header, .. } if expanded => {
                if !self.scanned_channels.contains(&header.channel_id) {
                    return Err(mcap::McapError::UnknownChannel(header.sequence, header.channel_id).into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct Reader {
    pub(super) options: memory::Options,
    pub(super) delivery: memory::Delivery,
    scratch: memory::Delivery,
    indexed: Option<sans_io::IndexedReader>,
    sorted: bool,
    order: u64,
    topics: Option<std::collections::BTreeSet<String>>,
    limit: Option<usize>,
    arena: sort_arena::Arena,
    indexed_summary: Option<Value>,
    indexed_summary_owner: Option<std::sync::Arc<mcap::Summary>>,
    parser: Option<sans_io::LinearReader>,
    input: Input,

    declarations: Declarations,
    expanded: bool,
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
    /// Copies independent snapshot input while preserving the sequential cursor.
    pub(super) fn copy_input(&mut self) -> Outcome<memory::Backing> {
        if self.failed { return Err("Reader failed".into()); }
        if !self.input.seekable() { return Err("Snapshot requires a seekable source".into()); }
        let pos = self.input.stream_position()?;
        let result = (|| -> Outcome<memory::Backing> {
            let length = usize::try_from(self.input.seek(SeekFrom::End(0))?)?;
            self.input.seek(SeekFrom::Start(0))?;
            let mut data = Vec::new();
            data.try_reserve_exact(length)?;
            data.resize(length, 0);
            self.input.read_exact(&mut data)?;
            Ok(memory::Backing::Owned { data })
        })();
        restored(result, self.input.seek(SeekFrom::Start(pos)))
    }

    pub(super) unsafe fn record_into(&mut self, offset: u64, dest: *mut u8,
        capacity: usize, opcode: *mut u8, out: &mut Response) -> Outcome<i32> {
        if self.failed {
            return Err("Reader failed".into());
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
            let start = self.input.stream_position()?;
            let end = self.input.seek(SeekFrom::End(0))?;
            if n as u64 > end.saturating_sub(start) {
                return Err("Record exceeds source length".into());
            }
            let status = if let Input::Map { mapping, .. } = &self.input {
                let start = usize::try_from(start)?;
                let data = &mapping[start..start + n];
                mcap::parse_record(h[0], data)?;
                super::record_access::copy_body(data, dest, capacity, out)?
            } else {
                self.input.seek(SeekFrom::Start(start))?;
                self.scratch.data.clear();
                self.scratch.reserve_scratch(n)?;
                self.scratch.data.resize(n, 0);
                self.input.read_exact(&mut self.scratch.data)?;
                mcap::parse_record(h[0], &self.scratch.data)?;
                super::record_access::copy_body(&self.scratch.data, dest, capacity, out)?
            };
            if !opcode.is_null() {
                *opcode = h[0];
            }
            Ok(status)
        })();
        let restored = self.input.seek(SeekFrom::Start(pos));
        self.scratch.release();
        super::restored(result, restored)
    }

    fn observe(&mut self, op: u8, data: &[u8]) -> Outcome<()> {
        self.count += 1;
        let record = mcap::parse_record(op, data)?;
        self.declarations.observe(&record, self.expanded)?;
        match record {
            records::Record::Schema { header, .. } if self.in_summary => {
                self.summary["schemaIds"].as_array_mut().unwrap().push(json!(header.id));
            }
            records::Record::Channel(c) if self.in_summary => {
                self.summary["channelIds"].as_array_mut().unwrap().push(json!(c.id));
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
            .declarations.channels
            .get(&h.channel_id)
            .ok_or(mcap::McapError::UnknownChannel(h.sequence, h.channel_id))?;
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
            return Ok(crate::protocol::status::END);
        }
        if let Some(mut indexed) = self.indexed.take() {
            let result = (|| loop {
                match indexed.next_shared_event().transpose()? {
                    None => {
                        self.ended = true;
                        return Ok(crate::protocol::status::END);
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::ReadChunkRequest {
                        offset,
                        length,
                    }) => {
                        if let Input::Map {
                            mapping, position, ..
                        } = &mut self.input
                        {
                            let start = usize::try_from(offset)?;
                            let end = start.checked_add(length).ok_or(mcap::McapError::BadIndex)?;
                            mapping.get(start..end).ok_or(mcap::McapError::BadIndex)?;
                            indexed.insert_shared_chunk_record_data(
                                offset,
                                mcap::storage::SharedBytes::external(mapping.clone(), start..end),
                            )?;
                            *position = end;
                        } else {
                            self.input.seek(SeekFrom::Start(offset))?;
                            self.scratch.data.clear();
                            self.scratch.reserve_scratch(length)?;
                            self.scratch.data.resize(length, 0);
                            self.input.read_exact(&mut self.scratch.data)?;
                            let shared = self.scratch.take_shared(0)?;
                            indexed.insert_shared_chunk_record_data(offset, shared)?;
                            self.scratch.release();
                        }
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::Message {
                        header,
                        data,
                    }) => {
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
                        self.delivery.shared = Some(data.clone());
                        return sink(self, records::op::MESSAGE, data.as_ref(), h);
                    }
                }
            })();
            self.indexed = if self.ended { None } else { Some(indexed) };
            return result;
        }
        let mut parser = self.parser.take().ok_or("Missing parser")?;
        let result = (|| loop {
            match parser.next_shared_event().transpose()? {
                None => {
                    self.ended = true;
                    return Ok(crate::protocol::status::END);
                }
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                    if let Input::Map { mapping, position } = &mut self.input {
                        if *position == mapping.len() {
                            parser.notify_read(0);
                        } else {
                            parser.supply_shared(mcap::storage::SharedBytes::external(
                                mapping.clone(),
                                *position..mapping.len(),
                            ));
                            *position = mapping.len();
                        }
                    } else {
                        io::feed_linear(&mut self.input, &mut parser, n)?;
                    }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record {
                    opcode,
                    data: shared,
                }) => {
                    let data = shared.as_ref();
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
                        self.delivery.shared = Some(shared.slice(22..shared.as_ref().len()));
                        return sink(self, opcode, &data, h);
                    }
                    self.delivery.shared = Some(shared.clone());
                    return sink(self, opcode, data, MessageHeader::default());
                }
            }
        })();
        self.parser = if self.ended { None } else { Some(parser) };
        result
    }

    unsafe fn read(&mut self, dest: *mut u8, capacity: usize, out: &mut Response) -> Outcome<i32> {
        if self.sorted && self.delivery.capture {
            self.delivery.shared = self.arena.read_shared(&mut self.delivery.header, out);
            if self.delivery.shared.is_none() {
                self.ended = true;
                return Ok(crate::protocol::status::END);
            }
            return Ok(crate::protocol::status::SUCCESS);
        }
        if self.sorted {
            let status = if let Some(sink) = self.delivery.sink {
                self.arena
                    .read_owned(sink, &mut self.delivery.header, out)?
            } else {
                self.arena
                    .read(dest, capacity, &mut self.delivery.header, out)?
            };
            self.delivery.opcode = records::op::MESSAGE;
            if status == crate::protocol::status::END {
                self.ended = true;
            }
            return Ok(status);
        }
        if self.delivery.active {
            out.value = self.delivery.bytes().len() as u64;
            if self.delivery.wanted == 0 || self.delivery.wanted == self.delivery.opcode {
                return self.delivery.retry(dest, capacity);
            }
            self.delivery.release();
        }
        loop {
            let status = self.next_with(|r, op, data, h| {
                r.delivery.opcode = op;
                r.delivery.header = h;
                out.value = data.len() as u64;
                if r.delivery.wanted != 0 && r.delivery.wanted != op {
                    return Ok(SKIP_RECORD);
                }
                r.delivery.deliver(data, dest, capacity)
            })?;
            if status != SKIP_RECORD {
                return Ok(status);
            }
        }
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
        let mut memory_options = memory::Options::parse(&v["options"])?;
        memory_options.sort = v["maxBufferedSortBytes"].as_u64();
        let mut reader = Reader {
            options: memory_options,
            delivery: memory::Delivery::default(),
            scratch: memory::Delivery::default(),
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
            indexed_summary_owner: None,
            parser: Some(sans_io::LinearReader::new_with_options(
                engine::linear_options(&v["options"])?
                    .with_emit_chunks(v["topLevel"].as_bool().unwrap_or(false)),
            )),
            input,
            declarations: Declarations::default(),
            expanded: !v["topLevel"].as_bool().unwrap_or(false),
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
                return Ok(crate::protocol::reader_open_status::BUFFERED_SORT_REQUIRED);
            }
            while reader.next_with(|r, _op, data, h| {
                let _ = data;
                let shared = r.delivery.shared.take().ok_or("Missing sort storage")?;
                r.arena.push_shared(h, shared, &r.options)?;
                Ok(crate::protocol::status::SUCCESS)
            })? != 1
            {}
            reader.arena.sort(reader.order == 1)?;
            reader.sorted = true;
            reader.ended = false;
        }
        *handle = Box::into_raw(Box::new(reader));
        Ok(crate::protocol::status::SUCCESS)
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
        let status = result?;
        if status == crate::protocol::status::END {
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
pub unsafe extern "C" fn fm_reader_owned(
    handle: *mut Reader,
    wanted: u8,
    sink: memory::Sink,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    // fm_reader_next catches panics and applies the same terminal-error cleanup.
    if let Some(r) = handle.as_mut() {
        r.delivery.sink = Some(sink);
        r.delivery.wanted = wanted;
    }
    let status = fm_reader_next(handle, ptr::null_mut(), 0, header, ptr::null_mut(), out);
    if let Some(r) = handle.as_mut() {
        r.delivery.sink = None;
        r.delivery.wanted = 0;
    }
    status
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
            crate::protocol::declaration_kind::SCHEMA => {
                let (h, d) = r.declarations.schemas.get(&id).ok_or_else(|| mcap::McapError::UnknownSchema(String::new(), id))?;
                respond(
                    out,
                    serde_json::to_vec(&json!({"id":h.id,"name":h.name,"encoding":h.encoding}))?,
                    d.clone(),
                    0,
                );
            }
            crate::protocol::declaration_kind::CHANNEL => {
                let c = r.declarations.channels.get(&id).ok_or(mcap::McapError::UnknownChannel(0, id))?;
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
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_reader_release(p: *mut Reader, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() { drop(Box::from_raw(p)); }
        Ok(crate::protocol::status::SUCCESS)
    })
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
        restored(result, self.input.seek(SeekFrom::Start(pos)))
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
        restored(result, self.input.seek(SeekFrom::Start(pos)))
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_summary(handle: *mut Reader, out: *mut Response) -> i32 {
    guard(out, |out| {
        let r = handle.as_mut().ok_or("Null reader")?;
        if let Some(s) = &r.indexed_summary {
            respond(out, serde_json::to_vec(s)?, vec![], 0);
            return Ok(crate::protocol::status::SUCCESS);
        }
        if r.ended {
            if r.summary_present {
                respond(out, serde_json::to_vec(&r.summary)?, vec![], 0);
            }
            return Ok(crate::protocol::status::SUCCESS);
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
                return Ok(crate::protocol::status::SUCCESS);
            }
            Err(e) => return Err(e),
        };
        if let Some(s) = summary {
            for (id, c) in &s.channels {
                r.declarations.channels.entry(*id).or_insert(records::Channel {
                    id: *id,
                    schema_id: c.schema.as_ref().map(|s| s.id).unwrap_or(0),
                    topic: c.topic.clone(),
                    message_encoding: c.message_encoding.clone(),
                    metadata: c.metadata.clone(),
                });
            }
            for (id, schema) in &s.schemas {
                r.declarations.schemas.entry(*id).or_insert((
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
        Ok(crate::protocol::status::SUCCESS)
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
        Ok(crate::protocol::status::SUCCESS)
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
        let mut declarations = Declarations::default();
        while let Some(e) = parser.next_event() {
            match e? {
                sans_io::LinearReadEvent::ReadRequest(n) => {
                    io::feed_linear(&mut input, &mut parser, n)?;
                }
                sans_io::LinearReadEvent::Record { opcode, data } => {
                    declarations.observe(&mcap::parse_record(opcode, data)?, true)?;
                    count += 1;
                }
            }
        }
        out.value = count;
        Ok(crate::protocol::status::SUCCESS)
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
        let safe = restored(safe, self.input.seek(SeekFrom::Start(position)))?;
        if !safe {
            return Ok(());
        }
        for (id, s) in &summary.schemas {
            self.declarations.schemas.insert(
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
            self.declarations.channels.insert(
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
        self.indexed_summary_owner = Some(std::sync::Arc::new(summary));
        Ok(())
    }
}

impl Reader {
    // A valid file may omit repeated schemas but retain repeated channels. Upstream's
    // SummaryReader cannot resolve those alone; scan declarations without moving our cursor.
    fn scan_summary(&mut self) -> Outcome<Option<Value>> {
        let pos = self.input.stream_position()?;
        // Summary fallback may populate lookup descriptions, but its traversal is not
        // progress of the caller's sequential validation session.
        let old_schemas = std::mem::take(&mut self.declarations.scanned_schemas);
        let old_channels = std::mem::take(&mut self.declarations.scanned_channels);
        let old_summary = std::mem::replace(&mut self.summary, empty_summary());
        let (old_present, old_in, old_count) = (self.summary_present, self.in_summary, self.count);
        self.summary_present = false;
        self.in_summary = false;
        let result = (|| -> Outcome<Option<Value>> {
            self.input.seek(SeekFrom::Start(0))?;
            let mut p = parser(false, self.limit);
            while let Some(e) = p.next_event() {
                match e? {
                    sans_io::LinearReadEvent::ReadRequest(n) => {
                        io::feed_linear(&mut self.input, &mut p, n)?;
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
        self.declarations.scanned_schemas = old_schemas;
        self.declarations.scanned_channels = old_channels;
        self.summary = old_summary;
        self.summary_present = old_present;
        self.in_summary = old_in;
        self.count = old_count;
        restored(result, self.input.seek(SeekFrom::Start(pos)))
    }
}


#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn summary_fallback_preserves_sequential_declaration_progress() {
        let mut writer = mcap::WriteOptions::default().use_chunks(false).repeat_schemas(false)
            .create(std::io::Cursor::new(Vec::new())).unwrap();
        let schema = writer.add_schema("schema", "raw", &[1]).unwrap();
        writer.add_channel(schema, "topic", "raw", &BTreeMap::new()).unwrap();
        writer.finish().unwrap();
        let data = writer.into_inner().into_inner();
        let path = std::env::temp_dir().join(format!("mcap-summary-progress-{}.mcap", std::process::id()));
        std::fs::write(&path, data).unwrap();
        let config = serde_json::to_vec(&json!({"path":path,"messages":false,"options":{}})).unwrap();
        let mut handle = ptr::null_mut();
        let mut response = Response::default();
        unsafe {
            assert_eq!(fm_reader_open(config.as_ptr(), config.len(), ptr::null(), &mut handle, &mut response), 0);
            let mut reader = Box::from_raw(handle);
            // Exercise nonempty progress as well as the fresh-session case. These IDs
            // are local bookkeeping only; the source remains immutable throughout.
            reader.declarations.scanned_schemas.insert(123);
            reader.declarations.scanned_channels.insert(456);
            let position = reader.input.stream_position().unwrap();
            let count = reader.count;
            assert!(reader.scan_summary().unwrap().is_some());
            assert_eq!(reader.declarations.scanned_schemas, [123].into_iter().collect());
            assert_eq!(reader.declarations.scanned_channels, [456].into_iter().collect());
            assert_eq!(reader.count, count);
            assert_eq!(reader.input.stream_position().unwrap(), position);
            assert!(reader.declarations.schemas.contains_key(&schema));
        }
        std::fs::remove_file(path).unwrap();
    }
}
