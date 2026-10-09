use super::summary::summary_json;
use super::{
    buffer_reader, bytes, guard, lease, memory, request, respond, MessageHeader, Outcome,
    Response,
};
use mcap::records;
use mcap::sans_io;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::ptr;
use std::sync::Arc;

pub(super) fn linear_options(v: &Value) -> Outcome<sans_io::LinearReaderOptions> {
    let mut o = sans_io::LinearReaderOptions::default();
    macro_rules! flag {
        ($key:literal, $field:ident) => {
            o.$field = v[$key].as_bool().unwrap_or(false);
        };
    }
    flag!("SkipStartMagic", skip_start_magic);
    flag!("SkipEndMagic", skip_end_magic);
    flag!("CheckFinishesAfterEndMagic", check_finishes_after_end_magic);
    flag!("EmitChunks", emit_chunks);
    flag!("ValidateChunkCrcs", validate_chunk_crcs);
    flag!("PrevalidateChunkCrcs", prevalidate_chunk_crcs);
    flag!("ValidateDataSectionCrc", validate_data_section_crc);
    flag!("ValidateSummarySectionCrc", validate_summary_section_crc);
    o.record_length_limit = v["RecordLengthLimit"]
        .as_u64()
        .map(usize::try_from)
        .transpose()?;
    Ok(o)
}

enum Engine {
    Inactive,
    Linear(sans_io::LinearReader),
    Summary(Option<sans_io::SummaryReader>),
    Indexed(sans_io::IndexedReader),
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_index_control(
    p: *mut EngineHandle,
    op: u32,
    value: u64,
    data: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed {
            return Err("Engine failed".into());
        }
        let Engine::Indexed(r) = &mut h.engine else {
            return Err("Indexed reader required".into());
        };
        match op {
            crate::protocol::indexed_control::INSERT_CHUNK => {
                r.insert_chunk_record_data(value, bytes(data, n)?)?;
                if h.waiting && h.event.kind == crate::protocol::engine_event::READ_CHUNK && h.event.offset == value {
                    h.waiting = false;
                }
            }
            crate::protocol::indexed_control::SET_RECORD_LENGTH_LIMIT => r.record_length_limit = Some(usize::try_from(value)?),
            crate::protocol::indexed_control::CLEAR_RECORD_LENGTH_LIMIT => r.record_length_limit = None,
            _ => return Err("Unknown indexed operation".into()),
        }
        Ok(crate::protocol::status::SUCCESS)
    });
    if status < crate::protocol::status::SUCCESS {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}

pub struct EngineHandle {
    lease_batch: Option<lease::Batch>,
    schemas: BTreeMap<u16, Arc<mcap::Schema<'static>>>,
    channels: BTreeMap<u16, Arc<mcap::Channel<'static>>>,
    engine: Engine,
    event: Event,
    delivery: memory::Delivery,
    waiting: bool,
    failed: bool,
    ended: bool,
    pub(super) summary: Option<Arc<mcap::Summary>>,
}

/// Copies declarations already observed during lease reading; never advances or faults the engine.
#[no_mangle]
pub unsafe extern "C" fn fm_engine_describe(
    p: *const EngineHandle,
    kind: u32,
    id: u16,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null engine")?;
        if h.failed { return Err("Engine failed".into()); }
        match kind {
            crate::protocol::declaration_kind::SCHEMA => {
                let schema = h.schemas.get(&id)
                    .ok_or_else(|| mcap::McapError::UnknownSchema(String::new(), id))?;
                respond(out, serde_json::to_vec(&serde_json::json!({
                    "id": schema.id, "name": schema.name, "encoding": schema.encoding
                }))?, schema.data.to_vec(), 0);
                Ok(crate::protocol::status::SUCCESS)
            }
            crate::protocol::declaration_kind::CHANNEL => buffer_reader::describe_channel(
                h.channels.get(&id).ok_or(mcap::McapError::UnknownChannel(0, id))?, out),
            _ => Err("Unknown declaration kind".into()),
        }
    })
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Event {
    kind: u32,
    opcode: u32,
    length: u64,
    offset: u64,
    origin: u32,
    reserved: u32,
    header: MessageHeader,
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_open(
    kind: u32,
    p: *const u8,
    n: usize,
    summary: *const EngineHandle,
    handle: *mut *mut EngineHandle,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let engine = match kind {
            crate::protocol::engine_kind::LINEAR => Engine::Linear(sans_io::LinearReader::new_with_options(linear_options(&v)?)),
            crate::protocol::engine_kind::SUMMARY => {
                let mut opts = sans_io::SummaryReaderOptions::default();
                if let Some(n) = v["FileSize"].as_u64() {
                    opts = opts.with_file_size(n);
                }
                if let Some(n) = v["RecordLengthLimit"].as_u64() {
                    opts = opts.with_record_length_limit(n.try_into()?);
                }
                Engine::Summary(Some(sans_io::SummaryReader::new_with_options(opts)))
            }
            crate::protocol::engine_kind::INDEXED => {
                let s = summary
                    .as_ref()
                    .and_then(|s| s.summary.as_ref())
                    .ok_or("Summary reader must finish with a summary")?;
                let mut opts = sans_io::IndexedReaderOptions::default();
                opts.start = v["StartTime"].as_u64();
                opts.end = v["EndTime"].as_u64();
                opts.order = match v["Order"].as_u64().unwrap_or(0) {
                    0 => sans_io::indexed_reader::ReadOrder::LogTime,
                    1 => sans_io::indexed_reader::ReadOrder::ReverseLogTime,
                    2 => sans_io::indexed_reader::ReadOrder::File,
                    _ => return Err("Invalid read order".into()),
                };
                opts.include_topics = v["Topics"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .or_else(|| {
                        v["Topic"]
                            .as_str()
                            .map(|t| [t.to_owned()].into_iter().collect())
                    });
                opts.record_length_limit = v["RecordLengthLimit"]
                    .as_u64()
                    .map(usize::try_from)
                    .transpose()?;
                Engine::Indexed(sans_io::IndexedReader::new_with_options(s, opts)?)
            }
            _ => return Err("Unknown engine".into()),
        };
        *handle = Box::into_raw(Box::new(EngineHandle {
            lease_batch: None,
            schemas: Default::default(),
            channels: Default::default(),
            engine,
            event: Event::default(),
            delivery: memory::Delivery::default(),
            waiting: false,
            failed: false,
            ended: false,
            summary: None,
        }));
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_next(
    p: *mut EngineHandle,
    dest: *mut u8,
    capacity: usize,
    event: *mut Event,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if event.is_null() {
            return Err("Null event".into());
        }
        if h.failed {
            return Err("Engine failed".into());
        }
        if !h.waiting && !h.ended {
            h.event = Event::default();
            h.delivery.data.clear();
            match &mut h.engine {
                Engine::Inactive => return Err("Engine failed".into()),
                Engine::Linear(r) => match r.next_shared_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                        h.event.kind = crate::protocol::engine_event::READ;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::linear_reader::SharedReadEvent::Record {
                        opcode,
                        data: shared,
                    }) => {
                        let data = shared.as_ref();
                        h.delivery.shared = Some(shared.clone());
                        h.event.kind = crate::protocol::engine_event::RECORD;
                        h.event.opcode = opcode as u32;

                        h.event.length = data.len() as u64;
                        *event = h.event;
                        let status = h.delivery.deliver(data, dest, capacity)?;
                        h.waiting = status == crate::protocol::status::BUFFER_TOO_SMALL;
                        return Ok(status);
                    }
                },
                Engine::Summary(r) => match r
                    .as_mut()
                    .ok_or("Summary consumed")?
                    .next_event()
                    .transpose()?
                {
                    None => {
                        h.summary = r.take().unwrap().finish().map(Arc::new);
                        h.ended = true;
                    }
                    Some(sans_io::SummaryReadEvent::ReadRequest(n)) => {
                        h.event.kind = crate::protocol::engine_event::READ;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::SummaryReadEvent::SeekRequest(s)) => {
                        h.event.kind = crate::protocol::engine_event::SEEK;
                        (h.event.origin, h.event.offset) = match s {
                            SeekFrom::Start(n) => (0, n),
                            SeekFrom::Current(n) => (1, n as u64),
                            SeekFrom::End(n) => (2, n as u64),
                        };
                    }
                },
                Engine::Indexed(r) => match r.next_shared_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::ReadChunkRequest {
                        offset,
                        length,
                    }) => {
                        h.event.kind = crate::protocol::engine_event::READ_CHUNK;
                        h.event.offset = offset;
                        h.event.length = length as u64;
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::Message {
                        header,
                        data: shared,
                    }) => {
                        let data = shared.as_ref();
                        h.delivery.shared = Some(shared.clone());
                        h.event.kind = crate::protocol::engine_event::MESSAGE;
                        h.event.length = data.len() as u64;

                        h.event.header = MessageHeader {
                            channel_id: header.channel_id,
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            reserved: 0,
                        };
                        *event = h.event;
                        let status = h.delivery.deliver(data, dest, capacity)?;
                        h.waiting = status == crate::protocol::status::BUFFER_TOO_SMALL;
                        return Ok(status);
                    }
                },
            }
            h.waiting = !h.ended;
        }
        if h.ended {
            h.delivery.discard();
            *event = Event { kind: crate::protocol::engine_event::END, ..Event::default() };
            return Ok(crate::protocol::status::END);
        }
        *event = h.event;
        if h.event.kind == crate::protocol::engine_event::RECORD || h.event.kind == crate::protocol::engine_event::MESSAGE {
            let status = h.delivery.retry(dest, capacity)?;
            if status == crate::protocol::status::BUFFER_TOO_SMALL {
                return Ok(crate::protocol::status::BUFFER_TOO_SMALL);
            }
            h.waiting = false;
        }
        Ok(crate::protocol::status::SUCCESS)
    });
    if status < crate::protocol::status::SUCCESS {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_feed(
    p: *mut EngineHandle,
    data: *const u8,
    n: usize,
    position: u64,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting {
            return Err("No pending input request".into());
        }
        let data = bytes(data, n)?;
        match (&mut h.engine, h.event.kind) {
            (Engine::Linear(r), crate::protocol::engine_event::READ) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.try_insert(n)?.copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), crate::protocol::engine_event::READ) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.insert(n).copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), crate::protocol::engine_event::SEEK) => r.notify_seeked(position),
            (Engine::Indexed(r), crate::protocol::engine_event::READ_CHUNK) => r.insert_chunk_record_data(position, data)?,
            _ => return Err("Unexpected input".into()),
        }
        h.waiting = false;
        Ok(crate::protocol::status::SUCCESS)
    });
    if status < crate::protocol::status::SUCCESS {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_summary(p: *const EngineHandle, out: *mut Response) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null engine")?;
        if !h.ended {
            return Err("Summary scan incomplete".into());
        }
        if let Some(s) = &h.summary {
            respond(out, serde_json::to_vec(&summary_json(s))?, vec![], 0);
        }
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_release(p: *mut EngineHandle, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_input_buffer(
    p: *mut EngineHandle,
    n: usize,
    data: *mut *mut u8,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting || h.event.kind != crate::protocol::engine_event::READ || n as u64 > h.event.length {
            return Err("No matching input request".into());
        }
        let Engine::Linear(r) = &mut h.engine else {
            return Err("Linear engine required".into());
        };
        let target = data.as_mut().ok_or("Null output")?;
        *target = ptr::null_mut();
        // Reserve the parser's complete request before a short asynchronous read.
        // Reserving only the managed read quantum would repeatedly relocate large records.
        *target = r.try_insert(usize::try_from(h.event.length)?)?.as_mut_ptr();
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_input_complete(
    p: *mut EngineHandle,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting || h.event.kind != crate::protocol::engine_event::READ || n as u64 > h.event.length {
            return Err("No matching input request".into());
        }
        let Engine::Linear(r) = &mut h.engine else {
            return Err("Linear engine required".into());
        };
        r.notify_read(n);
        h.waiting = false;
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_lease_step(
    p: *mut EngineHandle,
    count: usize,
    target: usize,
    output: *mut *mut lease::Batch,
    event: *mut Event,
    out: *mut Response,
) -> i32 {
    if !output.is_null() {
        *output = ptr::null_mut();
    }
    let status = guard(out, |out| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed
            || output.is_null()
            || event.is_null()
            || count == 0
            || count > 65536
            || target == 0
        {
            return Err("Invalid lease step".into());
        }
        *event = Event { kind: crate::protocol::engine_event::END, ..Event::default() };
        if h.ended {
            return Ok(crate::protocol::status::END);
        }
        if h.waiting {
            if h.event.kind != crate::protocol::engine_event::READ {
                return Err("Consume the pending record before switching to leases".into());
            }
            *event = h.event;
            return Ok(crate::protocol::status::SUCCESS);
        }
        let Engine::Linear(r) = &mut h.engine else {
            return Err("Linear engine required".into());
        };
        if h.lease_batch.is_none() {
            h.lease_batch = Some(lease::Batch::new(count)?);
        }
        let mut used = 0usize;
        loop {
            let next = r.next_shared_event().transpose()?;
            match next {
                None => {
                    h.ended = true;
                    break;
                }
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                    h.event = Event {
                        kind: crate::protocol::engine_event::READ,
                        length: n as u64,
                        ..Default::default()
                    };
                    h.waiting = true;
                    if h.lease_batch.as_ref().unwrap().messages.is_empty() {
                        *event = h.event;
                        return Ok(crate::protocol::status::SUCCESS);
                    }
                    break;
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data }) => {
                    match mcap::parse_record(opcode, data.as_ref())? {
                        records::Record::Message { header, .. } => {
                            if !h.channels.contains_key(&header.channel_id) {
                                return Err(mcap::McapError::UnknownChannel(
                                    header.sequence,
                                    header.channel_id,
                                )
                                .into());
                            }
                            let header = buffer_reader::native_header(&header);
                            let payload = data.slice(22..data.as_ref().len());
                            used += payload.as_ref().len();
                            let batch = h.lease_batch.as_mut().unwrap();
                            batch.messages.push(lease::Message {
                                header,
                                data: payload,
                            });
                            if batch.messages.len() >= count || used >= target {
                                break;
                            }
                        }
                        record => buffer_reader::BufferReader::observe(
                            &mut h.schemas,
                            &mut h.channels,
                            record,
                            &mut h.delivery,
                        )?,
                    }
                }
            }
        }
        let batch = h.lease_batch.take().unwrap();
        out.value = batch.messages.len() as u64;
        if !batch.messages.is_empty() {
            *output = lease::publish(batch);
            return Ok(crate::protocol::status::SUCCESS);
        }
        Ok(crate::protocol::status::END)
    });
    if status < crate::protocol::status::SUCCESS {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.lease_batch = None;
            h.engine = Engine::Inactive;
        }
    }
    status
}

const _: () = assert!(std::mem::size_of::<Event>() == 56);
const _: () = assert!(std::mem::offset_of!(Event, header) == 32);
