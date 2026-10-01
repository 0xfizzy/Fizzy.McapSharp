//! Private C ABI. No borrowed native memory crosses the public managed API.
mod buffer_reader;
mod batch;
mod lease;
mod budget;
mod budget_json;
#[cfg(test)]
mod coverage_tests;
mod errors;
mod response;
mod record_input;
mod summary_response;
mod extended;
mod io;
mod memory;
#[cfg(test)]
mod random_access;
mod chunk_cache;
mod sort_arena;
use io::{Callbacks, Input, Output};
use mcap::{records, sans_io};
use memmap2::Mmap;
use serde_json::{json, Value};
type SharedSummary = mcap::charged::weak::BudgetedArc<mcap::Summary>;
fn retain_summary(value: mcap::Summary, domain: &mcap::storage::BudgetRef, owner: mcap::storage::OwnerKind) -> Outcome<SharedSummary> {
    Ok(SharedSummary::new_with_owner_fixed(value, domain, mcap::storage::ResourceCategory::Declaration, owner)?)
}
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};
type Error = errors::NativeError;
type Outcome<T> = Result<T, Error>;
#[repr(C)]
#[derive(Default)]
pub struct Response {
    json: *mut u8,
    json_len: usize,
    data: *mut u8,
    data_len: usize,
    value: u64,
    error: errors::FixedError,
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
fn guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    if out.is_null() {
        return -1;
    }
    let out = unsafe { &mut *out };
    *out = Response::default();
    let status = match catch_unwind(AssertUnwindSafe(|| f(out))) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            if catch_unwind(AssertUnwindSafe(|| errors::encode_into(&mut out.error, e.as_ref()))).is_err() {
                out.error.panic();
            }
            -1
        }
        Err(_) => {
            out.error.panic();
            -1
        }
    };
    budget::fm_budget_dispatch();
    status
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
        .ok_or_else(|| Error::message(format_args!("Missing string: {k}")))
}
fn number(v: &Value, k: &str) -> Outcome<u64> {
    v[k].as_u64()
        .ok_or_else(|| Error::message(format_args!("Missing integer: {k}")))
}
fn map(v: &Value) -> Outcome<BTreeMap<String, String>> {
    Ok(serde_json::from_value(v.clone())?)
}
#[no_mangle]
pub extern "C" fn fm_abi_version() -> u32 {
    11
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_free(p: *mut u8, _n: usize) {
    response::free(p);
    budget::fm_budget_dispatch();
}

pub struct Writer {
    domain:mcap::storage::BudgetRef,
    inner: Option<mcap::Writer<Output>>,
    completed_output: Option<Output>,
    failed: bool,
    recoverable_errors: u32,
    attachment: Option<u64>,
    native_summary: Option<SharedSummary>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(w) = self.inner.take() {
            drop(w.into_inner());
        }
    }
}
#[cfg(test)]
fn options(v:&Value,seekable:bool)->Outcome<mcap::WriteOptions> {
    options_control(budget_json::Control::Existing(v),seekable,budget::parse(&v["memory"]["budget"])?)
}
fn options_control(v: budget_json::Control<'_>, seekable: bool, domain:mcap::storage::BudgetRef) -> Outcome<mcap::WriteOptions> {
    let compression = match v.string("compression")? {
        "none" => None,
        "lz4" => Some(mcap::Compression::Lz4),
        "zstd" => Some(mcap::Compression::Zstd),
        _ => return Err("Unknown compression".into()),
    };
    let mut o = mcap::WriteOptions::new()
        .memory_budget(domain)
        .compression(compression)
        .chunk_size(v.get("chunkSize").as_u64())
        .use_chunks(v.get("useChunks").as_bool().unwrap_or(true))
        .try_profile(v.string("profile")?)?
        .try_library(v.get("library").as_str().unwrap_or(mcap::LIBRARY_IDENTIFIER))?
        .disable_seeking(v.get("disableSeeking").as_bool().unwrap_or(!seekable));
    if !seekable && v.get("disableSeeking").as_bool() == Some(false) {
        return Err("Non-seekable output requires DisableSeeking".into());
    }
    macro_rules! flag {
        ($key:literal,$method:ident) => {
            if let Some(x) = v.get($key).as_bool() {
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
    if let Some(x) = v.get("compressionLevel").as_u64() {
        o = o.compression_level(x.try_into()?);
    }
    if let Some(x) = v.get("compressionThreads").as_u64() {
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
        let (document,domain)=budget_json::Document::configured(bytes(p,n)?,&["options","memory","budget","id"])?;
        let v=budget_json::Control::Charged(document.view());
        let settings=v.get("options");
        let recoverable_errors = if settings.contains_key("recoverableErrors") {
            settings.get("recoverableErrors").as_u64().filter(|n|n & !31==0)
                .ok_or("Invalid recovery flags")? as u32
        } else {31};
        let seekable=callbacks.as_ref().map(|c|c.seekable!=0).unwrap_or(true);
        let write_options=options_control(settings,seekable,domain.clone())?;
        let mut writer=mcap::charged::ChargedBox::new_fixed(Writer {
            inner: None,
            domain:domain.clone(),
            completed_output: None,
            failed: false,
            recoverable_errors,
            attachment: None,
            native_summary: None,
        },&domain,mcap::storage::ResourceCategory::Scratch)?;
        writer.charge_owner(mcap::storage::OwnerKind::Operation,true);
        let output = if let Some(c) = callbacks.as_ref() {
            Output::Stream(*c)
        } else {
            Output::File(
                File::options()
                    .write(true)
                    .read(true)
                    .create_new(true)
                    .open(v.string("path")?)?,
            )
        };
        writer.inner=Some(write_options.create(output)?);
        *handle=writer.into_raw_value();
        Ok(0)
    })
}
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
            Error::safe_rejection(e)
        } else {
            Error::from(e)
        }
    })
}
fn writer_guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    guard(out, |out| match f(out) {
        Err(e) if e.is_safe_rejection() => {
            errors::encode_into(&mut out.error, e.as_ref());
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
                    Error::safe_rejection(e)
                } else {
                    Error::from(e)
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
        if n == 0 {
            return writer_control(handle, op, budget_json::Control::Existing(&Value::Null), bytes(data, len)?, out);
        }
        let holder=handle.as_ref().ok_or("Null writer")?;
        if holder.failed { return Err("Writer failed".into()); }
        let domain=holder.domain.clone();
        let document=budget_json::Document::parse(bytes(p,n)?,&domain)?;
        writer_control(handle, op, budget_json::Control::Charged(document.view()), bytes(data, len)?, out)
    });
    writer_result(handle, status)
}
unsafe fn writer_control(
    handle: *mut Writer,
    op: u32,
    v: budget_json::Control<'_>,
    payload: &[u8],
    out: &mut Response,
) -> Outcome<i32> {
    let holder = handle.as_mut().ok_or("Null writer")?;
    if holder.failed {
        return Err("Writer failed".into());
    }
    if op == 13 {
        holder.completed_output.as_mut().ok_or("Complete must succeed first")?.sync_all()?;
        return Ok(0);
    }
    if op == 12 {
        if let Some(s) = &holder.native_summary {
            summary_response::respond(out, &holder.domain, s)?;
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
                if let Some(id) = v.get("id").as_u64() {
                    registration_result(
                        w.add_schema_with_id(
                            id.try_into()?,
                            v.string("name")?,
                            v.string("encoding")?,
                            payload,
                        ),
                        holder.recoverable_errors,
                        true,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_schema(v.string("name")?, v.string("encoding")?, payload),
                        holder.recoverable_errors,
                        true,
                        false,
                    )?
                }
            }) as u64
        }
        2 => {
            ({
                if let Some(id) = v.get("id").as_u64() {
                    registration_result(
                        w.add_channel_with_id_borrowed(
                            id.try_into()?,
                            v.number("schema_id")?.try_into()?,
                            v.string("topic")?,
                            v.string("encoding")?,
                            v.get("metadata").strings()?,
                        ),
                        holder.recoverable_errors,
                        false,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_channel_borrowed(
                            v.number("schema_id")?.try_into()?,
                            v.string("topic")?,
                            v.string("encoding")?,
                            v.get("metadata").strings()?,
                        ),
                        holder.recoverable_errors,
                        false,
                        false,
                    )?
                }
            }) as u64
        }
        4 => {
            w.write_metadata_borrowed(v.string("name")?,v.get("metadata").strings()?)?;
            0
        }
        5 => {
            w.attach_borrowed(v.number("log_time")?, v.number("create_time")?,
                v.string("name")?, v.string("media_type")?, payload)?;
            0
        }
        6 => {
            w.flush()?;
            0
        }
        7 => {
            let s = w.finish()?;
            holder.native_summary = Some(retain_summary(s, &holder.domain, mcap::storage::OwnerKind::Operation)?);
            holder.completed_output = Some(holder.inner.take().unwrap().into_inner());
            holder.completed_output.as_mut().unwrap().flush()?;
            0
        }
        8 => {
            let size = v.number("length")?;
            w.start_attachment_borrowed(size, v.number("log_time")?, v.number("create_time")?,
                v.string("name")?, v.string("media_type")?)?;
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
            let opts = if v.get("includeInChunks").as_bool().unwrap_or(false) {
                enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
            } else {
                enumset::EnumSet::new()
            };
            w.write_private_record(v.number("opcode")?.try_into()?, payload, opts)?;
            0
        }
        _ => return Err("Unknown writer operation".into()),
    };
    Ok(0)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_free(p: *mut Writer) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<Writer>::from_raw_value(p))));
    }
}

fn open_input(path: &str, domain: &mcap::storage::BudgetRef) -> Outcome<Input> {
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
        mapping: io::MappingOwner::new(mapping, f, domain)?,
        position: 0,
    })
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
    indexed_summary_owner: Option<SharedSummary>,
    parser: Option<sans_io::LinearReader>,
    input: Input,

    schemas: buffer_reader::SchemaTable,
    channels: buffer_reader::ChannelTable,
    summary: summary_response::Observed,
    summary_chunks: mcap::segmented::SharedSegmentedVec<mcap::shared_chunk_index::SharedChunkIndex>,
    summary_statistics: Option<mcap::shared_statistics::SharedStatistics>,
    summary_metadata: mcap::segmented::SharedSegmentedVec<mcap::shared_metadata_index::SharedMetadataIndex>,
    summary_attachments: mcap::segmented::SharedSegmentedVec<mcap::shared_attachment_index::SharedAttachmentIndex>,
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
fn parser(top: bool, limit: Option<usize>, domain: mcap::storage::BudgetRef) -> sans_io::LinearReader {
    let mut o = sans_io::LinearReaderOptions::default()
        .with_emit_chunks(top)
        .with_prevalidate_chunk_crcs(!top)
        .with_validate_data_section_crc(true)
        .with_validate_summary_section_crc(true)
        .with_check_finishes_after_end_magic(true);
    if let Some(n) = limit {
        o = o.with_record_length_limit(n);
    }
    sans_io::LinearReader::new_with_options_and_budget(o, domain)
}
impl Reader {
    fn observe(&mut self, op: u8, data: &[u8]) -> Outcome<()> {
        self.count += 1;
        if op == records::op::CHUNK_INDEX {
            let index = mcap::shared_chunk_index::SharedChunkIndex::read(
                data, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser,
            )?;
            if self.in_summary { self.summary_chunks.push_fixed(index)?; }
            return Ok(());
        }
        if op == records::op::ATTACHMENT_INDEX {
            let index = mcap::shared_attachment_index::SharedAttachmentIndex::read(
                data, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser,
            )?;
            if self.in_summary { self.summary_attachments.push_fixed(index)?; }
            return Ok(());
        }
        if op == records::op::METADATA_INDEX {
            let index = mcap::shared_metadata_index::SharedMetadataIndex::read(
                data, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser,
            )?;
            if self.in_summary { self.summary_metadata.push_fixed(index)?; }
            return Ok(());
        }
        if op == records::op::STATISTICS {
            let statistics = mcap::shared_statistics::SharedStatistics::read(
                &mut std::io::Cursor::new(data), self.delivery.options.domain.clone(),
                mcap::storage::OwnerKind::Parser,
            )?;
            if self.in_summary { self.summary_statistics = Some(statistics); }
            return Ok(());
        }
        match op {
            records::op::SCHEMA => {
                let schema = mcap::shared_declarations::SharedSchema::read(
                    data, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser)?;
                if schema.id == 0 { return Err("Invalid schema ID".into()); }
                if let Some(old) = self.schemas.get(&schema.id) {
                    if old != &schema { return Err("Conflicting schema".into()); }
                }
                if self.in_summary { self.summary.schemas.push_fixed(schema.id)?; }
                if !self.schemas.contains_key(&schema.id) { self.schemas.insert_fixed(schema.id, schema)?; }
                return Ok(());
            }
            records::op::CHANNEL => {
                let channel = mcap::shared_declarations::SharedChannel::read_record(
                    data, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser)?;
                if self.messages && channel.schema_id != 0 && !self.schemas.contains_key(&channel.schema_id) {
                    return Err("Unknown schema".into());
                }
                if let Some(old) = self.channels.get(&channel.id) {
                    if !old.same_declaration(&channel) { return Err("Conflicting channel".into()); }
                }
                if self.in_summary { self.summary.channels.push_fixed(channel.id)?; }
                if !self.channels.contains_key(&channel.id) { self.channels.insert_fixed(channel.id, channel)?; }
                return Ok(());
            }
            _ => {}
        }
        record_input::validate(op, data, &self.delivery.options.domain)?;
        match op {
            records::op::DATA_END => self.in_summary = true,
            records::op::FOOTER => {
                let records::Record::Footer(f) = mcap::parse_record(op,data)? else {unreachable!()};
                self.summary_present = f.summary_start != 0;
            }
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
            indexed.set_memory_budget(self.delivery.options.domain.clone())?;
            let result = (|| loop {
                match indexed.next_shared_event().transpose()? {
                    None => {
                        self.ended = true;
                        return Ok(1);
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::ReadChunkRequest { offset, length }) => {
                        if let Input::Map {
                            mapping, position, ..
                        } = &mut self.input
                        {
                            let start = usize::try_from(offset)?;
                            let end = start.checked_add(length).ok_or(mcap::McapError::BadIndex)?;
                            mapping.get(start..end).ok_or(mcap::McapError::BadIndex)?;
                            indexed.insert_shared_chunk_record_data(offset, mapping.shared(start..end))?;
                            *position = end;
                        } else {
                            self.input.seek(SeekFrom::Start(offset))?;
                            self.scratch.data.clear();
                            let share_input = indexed.can_share_chunk_input(offset)?;
                            if share_input { self.scratch.reserve_input(length)?; }
                            else { self.scratch.reserve_scratch(length)?; }
                            self.scratch.data.resize(length, 0);
                            self.update_memory_peak();
                            self.input.read_exact(&mut self.scratch.data)?;
                            if share_input {
                                let data = self.scratch.take_shared(0)?;
                                indexed.insert_shared_chunk_record_data(offset, data)?;
                            } else {
                                indexed.insert_chunk_record_data(offset, &self.scratch.data)?;
                            }
                            self.scratch.stats.copied += length as u64;
                            self.scratch.options.domain.copy_bytes(mcap::storage::CopyKind::Input,length);
                            self.scratch.release();
                        }
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::Message { header, data }) => {
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
                        self.delivery.shared=Some(data.clone());
                        return sink(self, records::op::MESSAGE, data.as_ref(), h);
                    }
                }
            })();
            self.indexed = if self.ended { None } else { Some(indexed) };
            return result;
        }
        let mut parser = self.parser.take().ok_or("Missing parser")?;
        parser.set_memory_budget(self.delivery.options.domain.clone())?;
        let result = (|| loop {
            match parser.next_shared_event().transpose()? {
                None => {
                    self.ended = true;
                    return Ok(1);
                }
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                    if let Input::Map { mapping, position } = &mut self.input {
                        if *position == mapping.len() { parser.notify_read(0); }
                        else { parser.supply_shared(mapping.shared(*position..mapping.len())); *position=mapping.len(); }
                    } else {
                        let n = self.input.read(parser.try_insert(n.min(65536))?)?;
                        self.delivery.stats.copied += n as u64;
                        parser.notify_read(n);
                    }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data: shared }) => {
                    let data=shared.as_ref();
                    self.observe(opcode, data).map_err(budget::after_advance)?;
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
                        self.delivery.shared=Some(shared.slice(22..shared.as_ref().len()));
                        return sink(self, opcode, &data, h);
                    }
                    self.delivery.shared=Some(shared.clone());
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
        if self.sorted && self.delivery.capture {
            self.delivery.shared=self.arena.read_shared(&mut self.delivery.header,out);
            if self.delivery.shared.is_none() { self.ended=true; return Ok(1); }
            return Ok(0);
        }
        if self.sorted {
            let before = self.arena.stats.copied;
            let status = if let Some(sink) = self.delivery.sink {
                self.arena.read_owned(sink, &mut self.delivery.header, out)?
            } else {
                self.arena.read(dest, capacity, &mut self.delivery.header, out)?
            };
            self.delivery.options.domain.copy_bytes(mcap::storage::CopyKind::Delivery,(self.arena.stats.copied-before) as usize);
            self.delivery.opcode = records::op::MESSAGE;
            if status == 1 {
                self.ended = true;
            }
            return Ok(status);
        }
        if self.delivery.active {
            out.value = self.delivery.pending_len() as u64;
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
                if r.delivery.wanted != 0 && r.delivery.wanted != op { return Ok(3); }
                r.delivery.deliver(data, dest, capacity)
            })?;
            if status != 3 { return Ok(status); }
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
        let memory_options=memory::Options::parse(&v["options"]["Memory"])?;
        let input = if let Some(c) = callbacks.as_ref() {
            Input::Stream(*c)
        } else {
            open_input(string(&v, "path")?, &memory_options.domain)?
        };
        let limit = v["recordLengthLimit"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
        let reader = Reader {
            delivery: memory::Delivery::new(memory_options.clone()),
            memory_peak: 0,
            scratch: memory::Delivery::new(memory_options.clone()),
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
            indexed_summary_owner:None,
            parser: Some(sans_io::LinearReader::new_with_options_and_budget(
                extended::linear_options(&v["options"])?
                    .with_emit_chunks(v["topLevel"].as_bool().unwrap_or(false)),
                memory_options.domain.clone(),
            )),
            input,
            schemas: buffer_reader::SchemaTable::new_owned(memory_options.domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            channels: buffer_reader::ChannelTable::new_owned(memory_options.domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            summary: summary_response::Observed::new(&memory_options.domain),
            summary_statistics: None,
            summary_chunks: mcap::segmented::SharedSegmentedVec::new(memory_options.domain.clone()),
            summary_metadata: mcap::segmented::SharedSegmentedVec::new(memory_options.domain.clone()),
            summary_attachments: mcap::segmented::SharedSegmentedVec::new(memory_options.domain.clone()),
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
        let mut reader=mcap::charged::ChargedBox::new_fixed(reader,&memory_options.domain,mcap::storage::ResourceCategory::Scratch)?;
        reader.charge_owner(mcap::storage::OwnerKind::Parser,true);
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
                let _=data;
                let shared=r.delivery.shared.take().ok_or("Missing sort storage")?;
                r.arena.push_shared(h, shared, &r.delivery.options)?;
                r.update_memory_peak();
                Ok(0)
            })? != 1
            {}
            let descending=reader.order == 1;
            reader.arena.sort(descending);
            reader.sorted = true;
            reader.ended = false;
        }
        *handle = reader.into_raw_value();
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
        let status = match result { Err(ref e) if r.delivery.capture && budget::unavailable(e) => {
            r.delivery.retry_capacity=budget::requested_capacity(e).ok_or("Missing retry capacity")?;
            return Ok(4);
        }, other=>other? };
        if status == 1 {
            r.delivery.discard();
            r.scratch.discard();
            out.value = r.count;
            if !header.is_null() {
                (*header).reserved = if r.indexed_summary_owner.is_some() || r.sorted {
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
    handle: *mut Reader, wanted: u8, sink: memory::Sink,
    header: *mut MessageHeader, out: *mut Response,
) -> i32 {
    // fm_reader_next catches panics and applies the same terminal-error cleanup.
    if let Some(r) = handle.as_mut() { r.delivery.sink = Some(sink); r.delivery.wanted = wanted; }
    let status = fm_reader_next(handle, ptr::null_mut(), 0, header, ptr::null_mut(), out);
    if let Some(r) = handle.as_mut() { r.delivery.sink = None; r.delivery.wanted = 0; }
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
            1 => {
                let h = r.schemas.get(&id).ok_or("Unknown schema")?;
                response::schema(out, &r.delivery.options.domain, h)?;
            }
            2 => {
                let c = r.channels.get(&id).ok_or("Unknown channel")?;
                response::channel(out, &r.delivery.options.domain, c, false)?;
            }
            _ => return Err("Unknown description".into()),
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_reader_free(p: *mut Reader) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<Reader>::from_raw_value(p))));
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
            let mut s = sans_io::SummaryReader::new_with_options_and_budget(opts, self.delivery.options.domain.clone());
            while let Some(e) = s.next_event() {
                match e? {
                    sans_io::SummaryReadEvent::ReadRequest(n) => {
                        let n = self.input.read(s.try_insert(n.min(65536))?)?;
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
    fn record_at(&mut self, offset: u64) -> Outcome<(u8, record_input::Body)> {
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
            let data = record_input::Body::read(&mut self.input, body_start, n, &self.delivery.options)?;
            record_input::validate(h[0], data.as_ref(), &self.delivery.options.domain)?;
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
        if let Some(s) = &r.indexed_summary_owner {
            summary_response::respond(out, &r.delivery.options.domain, s)?;
            return Ok(0);
        }
        if r.ended {
            if r.summary_present {
                r.respond_summary(out)?;
            }
            return Ok(0);
        }
        let summary = match r.read_summary() {
            Ok(s) => s,
            Err(e)
                if e.downcast_ref::<mcap::McapError>()
                    .is_some_and(|e| matches!(e, mcap::McapError::UnknownSchema(..))) =>
            {
                r.scan_summary(out)?;
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        if let Some(s) = summary {
            for (id, c) in s.channels.iter() {
                if !r.channels.contains_key(&id) { r.channels.insert_fixed(id, c.clone())?; }
            }
            for (id, schema) in s.schemas.iter() {
                if !r.schemas.contains_key(&id) { r.schemas.insert_fixed(id, schema.clone())?; }
            }
            summary_response::respond(out, &r.delivery.options.domain, &s)?;
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
        response::write(out, &r.delivery.options.domain, |_| Ok(()), data.as_ref(), op as u64)?;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_validate(p: *const u8, n: usize, out: *mut Response) -> i32 {
    guard(out, |out| {
        let domain = mcap::storage::BudgetRef::try_default()?;
        let document=budget_json::Document::parse(bytes(p,n)?,&domain)?;
        let v=document.view();
        let mut input = open_input(v.get("path").as_str().ok_or("Missing string: path")?, &domain)?;
        let limit = v.get("recordLengthLimit")
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
        let mut parser = parser(false, limit, domain.clone());
        let mut count = 0;
        while let Some(e) = parser.next_event() {
            match e? {
                sans_io::LinearReadEvent::ReadRequest(n) => {
                    let n = input.read(parser.try_insert(n.min(65536))?)?;
                    parser.notify_read(n);
                }
                sans_io::LinearReadEvent::Record { opcode, data } => {
                    record_input::validate(opcode, data, &domain)?;
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
            let mut chunks = mcap::segmented::BudgetedSegmentedVec::new(
                self.delivery.options.domain.clone(),mcap::storage::ResourceCategory::Index);
            chunks.owner_reference(mcap::storage::OwnerKind::Operation,true);
            loop {
                let record_offset = self.input.stream_position()?;
                let mut h = [0; 9];
                self.input.read_exact(&mut h)?;
                if h[0] == records::op::MESSAGE {
                    return Ok(false);
                }
                if h[0] == records::op::CHUNK {
                    let length = u64::from_le_bytes(h[1..].try_into()?);
                    chunks.push_fixed((record_offset,
                        length.checked_add(9).ok_or(mcap::McapError::BadIndex)?,false))?;
                }
                if h[0] == records::op::FOOTER {
                    if chunks.len() != summary.chunk_indexes.len() {
                        return Ok(false);
                    }
                    for index in &summary.chunk_indexes {
                        let position=chunks.binary_search_by_key(&index.chunk_start_offset, |row|row.0)
                            .map_err(|_|mcap::McapError::BadIndex)?;
                        let row=&mut chunks[position];
                        if row.2 || row.1 != index.chunk_length {
                            return Err(mcap::McapError::BadIndex.into());
                        }
                        row.2=true;
                    }
                    // Equal cardinality and no duplicate matches establish complete coverage.
                    return Ok(true);
                }
                let n = u64::from_le_bytes(h[1..].try_into()?);
                self.input.seek(SeekFrom::Current(i64::try_from(n)?))?;
            }
        })();
        self.input.seek(SeekFrom::Start(position))?;
        if !safe? {
            return Ok(());
        }
        for (id, schema) in summary.schemas.iter() {
            self.schemas.insert_fixed(id, schema.clone())?;
        }
        for (id, channel) in summary.channels.iter() {
            self.channels.insert_fixed(id, channel.clone())?;
        }
        let mut opts = sans_io::IndexedReaderOptions::default();
        opts.start = self.start;
        opts.end = self.end;
        opts.order = match self.order {
            0 => sans_io::indexed_reader::ReadOrder::LogTime,
            1 => sans_io::indexed_reader::ReadOrder::ReverseLogTime,
            _ => sans_io::indexed_reader::ReadOrder::File,
        };
        opts.record_length_limit = self.limit;
        let filtered=self.topics.is_some() || self.topic.is_some();
        let include=filtered.then_some(|topic:&str| {
            if let Some(topics)=&self.topics {topics.contains(topic)}
            else {self.topic.as_deref()==Some(topic)}
        });
        self.indexed = Some(sans_io::IndexedReader::new_with_topic_filter(&summary, opts,
            self.delivery.options.domain.clone(),include)?);
        self.indexed_summary_owner=Some(retain_summary(summary, &self.delivery.options.domain, mcap::storage::OwnerKind::Parser)?);
        Ok(())
    }
}

// These are private C ABI layouts on all supported 64-bit targets, independent of Rust record layouts.
const _: () = assert!(std::mem::size_of::<MessageHeader>() == 24);
const _: () = assert!(std::mem::offset_of!(MessageHeader, log_time) == 8);
const _: () = assert!(std::mem::size_of::<Response>() == 4144);
const _: () = assert!(std::mem::size_of::<Callbacks>() == 48);

impl Reader {
    // A valid file may omit repeated schemas but retain repeated channels. Upstream's
    // SummaryReader cannot resolve those alone; scan declarations without moving our cursor.
    fn respond_summary(&self, out: &mut Response) -> Outcome<()> {
        summary_response::View {statistics:self.summary_statistics.as_ref(),chunks:&self.summary_chunks,
            attachments:&self.summary_attachments,metadata:&self.summary_metadata}
            .respond(out,&self.delivery.options.domain,&self.summary)
    }
    fn scan_summary(&mut self, out: &mut Response) -> Outcome<()> {
        let pos = self.input.stream_position()?;
        let old_summary = std::mem::replace(&mut self.summary, summary_response::Observed::new(&self.delivery.options.domain));
        let old_statistics = self.summary_statistics.take();
        let old_chunks = std::mem::replace(&mut self.summary_chunks,
            mcap::segmented::SharedSegmentedVec::new(self.delivery.options.domain.clone()));
        let old_attachments = std::mem::replace(&mut self.summary_attachments,
            mcap::segmented::SharedSegmentedVec::new(self.delivery.options.domain.clone()));
        let old_metadata = std::mem::replace(&mut self.summary_metadata,
            mcap::segmented::SharedSegmentedVec::new(self.delivery.options.domain.clone()));
        let (old_present, old_in, old_count) = (self.summary_present, self.in_summary, self.count);
        self.summary_present = false;
        self.in_summary = false;
        let result = (|| -> Outcome<()> {
            self.input.seek(SeekFrom::Start(0))?;
            let mut p = parser(false, self.limit, self.delivery.options.domain.clone());
            while let Some(e) = p.next_event() {
                match e? {
                    sans_io::LinearReadEvent::ReadRequest(n) => {
                        let n = self.input.read(p.try_insert(n.min(65536))?)?;
                        p.notify_read(n);
                    }
                    sans_io::LinearReadEvent::Record { opcode, data } => {
                        self.observe(opcode, data).map_err(budget::after_advance)?
                    }
                }
            }
            Ok(())
        })();
        // Restore the cursor before publishing owned response buffers. A failed
        // seek must not leave a successful response attached to an error.
        let restored = self.input.seek(SeekFrom::Start(pos));
        let result = result.and_then(|()| {
            restored?;
            if self.summary_present { self.respond_summary(out)?; }
            Ok(())
        });
        self.summary = old_summary;
        self.summary_statistics = old_statistics;
        self.summary_chunks = old_chunks;
        self.summary_attachments = old_attachments;
        self.summary_metadata = old_metadata;
        self.summary_present = old_present;
        self.in_summary = old_in;
        self.count = old_count;
        result
    }
}

#[cfg(test)]
mod memory_probe;
