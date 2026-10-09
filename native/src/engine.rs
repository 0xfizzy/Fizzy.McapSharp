use super::{
    buffer_reader, bytes, guard, lease, memory, request, respond, summary_json, MessageHeader,
    Outcome, Response,
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
            0 => {
                r.insert_chunk_record_data(value, bytes(data, n)?)?;
                if h.waiting && h.event.kind == 5 && h.event.offset == value {
                    h.waiting = false;
                }
            }
            1 => r.record_length_limit = Some(usize::try_from(value)?),
            2 => r.record_length_limit = None,
            _ => return Err("Unknown indexed operation".into()),
        }
        Ok(0)
    });
    if status < 0 {
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
            0 => Engine::Linear(sans_io::LinearReader::new_with_options(linear_options(&v)?)),
            1 => {
                let mut opts = sans_io::SummaryReaderOptions::default();
                if let Some(n) = v["FileSize"].as_u64() {
                    opts = opts.with_file_size(n);
                }
                if let Some(n) = v["RecordLengthLimit"].as_u64() {
                    opts = opts.with_record_length_limit(n.try_into()?);
                }
                Engine::Summary(Some(sans_io::SummaryReader::new_with_options(opts)))
            }
            2 => {
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
        Ok(0)
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
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::linear_reader::SharedReadEvent::Record {
                        opcode,
                        data: shared,
                    }) => {
                        let data = shared.as_ref();
                        h.delivery.shared = Some(shared.clone());
                        h.event.kind = 3;
                        h.event.opcode = opcode as u32;

                        h.event.length = data.len() as u64;
                        *event = h.event;
                        let status = h.delivery.deliver(data, dest, capacity)?;
                        h.waiting = status == 2;
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
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::SummaryReadEvent::SeekRequest(s)) => {
                        h.event.kind = 2;
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
                        h.event.kind = 5;
                        h.event.offset = offset;
                        h.event.length = length as u64;
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::Message {
                        header,
                        data: shared,
                    }) => {
                        let data = shared.as_ref();
                        h.delivery.shared = Some(shared.clone());
                        h.event.kind = 4;
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
                        h.waiting = status == 2;
                        return Ok(status);
                    }
                },
            }
            h.waiting = !h.ended;
        }
        if h.ended {
            h.delivery.discard();
            *event = Event::default();
            return Ok(1);
        }
        *event = h.event;
        if h.event.kind == 3 || h.event.kind == 4 {
            let status = h.delivery.retry(dest, capacity)?;
            if status == 2 {
                return Ok(2);
            }
            h.waiting = false;
        }
        Ok(0)
    });
    if status < 0 {
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
            (Engine::Linear(r), 1) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.try_insert(n)?.copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), 1) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.insert(n).copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), 2) => r.notify_seeked(position),
            (Engine::Indexed(r), 5) => r.insert_chunk_record_data(position, data)?,
            _ => return Err("Unexpected input".into()),
        }
        h.waiting = false;
        Ok(0)
    });
    if status < 0 {
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
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_release(p: *mut EngineHandle, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(0)
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
        if h.failed || !h.waiting || h.event.kind != 1 || n as u64 > h.event.length {
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
        Ok(0)
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
        if h.failed || !h.waiting || h.event.kind != 1 || n as u64 > h.event.length {
            return Err("No matching input request".into());
        }
        let Engine::Linear(r) = &mut h.engine else {
            return Err("Linear engine required".into());
        };
        r.notify_read(n);
        h.waiting = false;
        Ok(0)
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
        *event = Event::default();
        if h.ended {
            return Ok(1);
        }
        if h.waiting {
            if h.event.kind != 1 {
                return Err("Consume the pending record before switching to leases".into());
            }
            *event = h.event;
            return Ok(0);
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
                        kind: 1,
                        length: n as u64,
                        ..Default::default()
                    };
                    h.waiting = true;
                    if h.lease_batch.as_ref().unwrap().messages.is_empty() {
                        *event = h.event;
                        return Ok(0);
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
            return Ok(0);
        }
        Ok(1)
    });
    if status < 0 {
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
