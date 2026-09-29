use super::*;
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
        }
    }
    status
}
pub struct EngineHandle {
    engine: Engine,
    event: Event,
    pending: Vec<u8>,
    waiting: bool,
    failed: bool,
    ended: bool,
    summary: Option<mcap::Summary>,
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
            engine,
            event: Event::default(),
            pending: Vec::new(),
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
            h.pending.clear();
            match &mut h.engine {
                Engine::Linear(r) => match r.next_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::LinearReadEvent::ReadRequest(n)) => {
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::LinearReadEvent::Record { opcode, data }) => {
                        h.event.kind = 3;
                        h.event.opcode = opcode as u32;
                        h.pending.extend_from_slice(data);
                        h.event.length = data.len() as u64;
                    }
                },
                Engine::Summary(r) => match r
                    .as_mut()
                    .ok_or("Summary consumed")?
                    .next_event()
                    .transpose()?
                {
                    None => {
                        h.summary = r.take().unwrap().finish();
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
                Engine::Indexed(r) => match r.next_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::IndexedReadEvent::ReadChunkRequest { offset, length }) => {
                        h.event.kind = 5;
                        h.event.offset = offset;
                        h.event.length = length as u64;
                    }
                    Some(sans_io::IndexedReadEvent::Message { header, data }) => {
                        h.event.kind = 4;
                        h.event.length = data.len() as u64;
                        h.pending.extend_from_slice(data);
                        h.event.header = MessageHeader {
                            channel_id: header.channel_id,
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            reserved: 0,
                        };
                    }
                },
            }
            h.waiting = !h.ended;
        }
        if h.ended {
            *event = Event::default();
            return Ok(1);
        }
        *event = h.event;
        if h.event.kind == 3 || h.event.kind == 4 {
            if capacity < h.pending.len() {
                return Ok(2);
            }
            if !h.pending.is_empty() {
                if dest.is_null() {
                    return Err("Null destination".into());
                }
                ptr::copy_nonoverlapping(h.pending.as_ptr(), dest, h.pending.len());
            }
            h.waiting = false;
        }
        Ok(0)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
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
                r.insert(n).copy_from_slice(data);
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
pub unsafe extern "C" fn fm_engine_free(p: *mut EngineHandle) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}

pub struct PreparedChannel(Arc<mcap::Channel<'static>>);
pub struct PreparedOperation {
    op: u32,
    args: Value,
    data: Vec<u8>,
}
#[no_mangle]
pub unsafe extern "C" fn fm_operation_prepare(
    op: u32,
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    handle: *mut *mut PreparedOperation,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        if !matches!(op, 1 | 2 | 4 | 5 | 8) {
            return Err("Unsupported prepared operation".into());
        }
        *handle = Box::into_raw(Box::new(PreparedOperation {
            op,
            args: request(p, n)?,
            data: bytes(data, len)?.to_vec(),
        }));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_operation_free(p: *mut PreparedOperation) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_prepared(
    p: *mut Writer,
    operation: *const PreparedOperation,
    data: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |out| {
        let op = operation.as_ref().ok_or("Null operation")?;
        writer_control(
            p,
            op.op,
            &op.args,
            if op.op == 1 {
                &op.data
            } else {
                bytes(data, n)?
            },
            out,
        )
    });
    writer_result(p, status)
}

pub struct Snapshot {
    data: Vec<u8>,
    summary: Option<mcap::Summary>,
    messages: VecDeque<(MessageHeader, Vec<u8>)>,
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_summary(p: *const Snapshot, out: *mut Response) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        if let Some(s) = &h.summary {
            respond(out, serde_json::to_vec(&summary_json(s))?, vec![], 0);
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_bytes(
    data: *const u8,
    n: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let data = bytes(data, n)?.to_vec();
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            data,
            summary,
            messages: VecDeque::new(),
        }));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_footer(
    data: *const u8,
    n: usize,
    dest: *mut u8,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let f = mcap::read::footer(bytes(data, n)?)?;
        let (_, b) = buffer_reader::encode(records::Record::Footer(f))?;
        copy_body(&b, dest, 20, out)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_offset(
    offset: u64,
    compression: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let index = records::ChunkIndex {
            message_start_time: 0,
            message_end_time: 0,
            chunk_start_offset: offset,
            chunk_length: 0,
            message_index_offsets: BTreeMap::new(),
            message_index_length: 0,
            compression: std::str::from_utf8(bytes(compression, n)?)?.into(),
            compressed_size: 0,
            uncompressed_size: 0,
        };
        out.value = index.compressed_data_offset()?;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_summary_records(
    kind: u32,
    p: *const std::ffi::c_void,
    handle: *mut *mut buffer_reader::BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() || p.is_null() {
            return Err("Null handle".into());
        }
        *handle = ptr::null_mut();
        let summary = match kind {
            0 => &(*(p as *const Snapshot)).summary,
            1 => &(*(p as *const EngineHandle)).summary,
            2 => &(*(p as *const Writer)).native_summary,
            _ => return Err("Unknown summary source".into()),
        };
        *handle = Box::into_raw(Box::new(buffer_reader::summary_records(
            summary.as_ref().ok_or("No summary available")?,
        )?));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_channel(
    p: *const Snapshot,
    id: u16,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        let s = h.summary.as_ref().ok_or("No summary")?;
        buffer_reader::describe_channel(
            s.channels
                .get(&id)
                .ok_or(mcap::McapError::UnknownChannel(0, id))?,
            out,
        )
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_open(
    reader: *mut Reader,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let r = reader.as_mut().ok_or("Null reader")?;
        if !r.input.seekable() {
            return Err("Snapshot requires a seekable source".into());
        }
        let pos = r.input.stream_position()?;
        let result = (|| -> Outcome<Vec<u8>> {
            r.input.seek(SeekFrom::Start(0))?;
            let mut data = Vec::new();
            r.input.read_to_end(&mut data)?;
            Ok(data)
        })();
        r.input.seek(SeekFrom::Start(pos))?;
        let data = result?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            data,
            summary,
            messages: VecDeque::new(),
        }));
        Ok(0)
    })
}
fn own_message(m: mcap::Message<'_>) -> (MessageHeader, Vec<u8>) {
    (
        MessageHeader {
            channel_id: m.channel.id,
            sequence: m.sequence,
            log_time: m.log_time,
            publish_time: m.publish_time,
            reserved: 0,
        },
        m.data.into_owned(),
    )
}
pub(super) unsafe fn copy_body(
    data: &[u8],
    dest: *mut u8,
    capacity: usize,
    out: &mut Response,
) -> Outcome<i32> {
    out.value = data.len() as u64;
    if capacity < data.len() {
        return Ok(2);
    }
    if !data.is_empty() {
        if dest.is_null() {
            return Err("Null destination".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
    }
    Ok(0)
}
fn record_body(data: &[u8], offset: u64, expected: u8) -> Outcome<&[u8]> {
    let offset = usize::try_from(offset)?;
    let h = data
        .get(offset..)
        .and_then(|d| d.get(..9))
        .ok_or(mcap::McapError::BadIndex)?;
    if h[0] != expected {
        return Err(mcap::McapError::BadIndex.into());
    }
    let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
    Ok(data
        .get(offset + 9..)
        .and_then(|d| d.get(..n))
        .ok_or(mcap::McapError::BadIndex)?)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_call(
    p: *mut Snapshot,
    op: u32,
    offset: u64,
    argument: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        if !header.is_null() {
            *header = MessageHeader::default();
        }
        if op == 7 {
            let Some((msg, data)) = h.messages.front() else {
                return Ok(1);
            };
            if !header.is_null() {
                *header = *msg;
            }
            let status = copy_body(data, dest, capacity, out)?;
            if status == 0 {
                h.messages.pop_front();
            }
            return Ok(status);
        }
        if op == 6 {
            let f = mcap::read::footer(&h.data)?;
            let mut b = Vec::new();
            b.extend_from_slice(&f.summary_start.to_le_bytes());
            b.extend_from_slice(&f.summary_offset_start.to_le_bytes());
            b.extend_from_slice(&f.summary_crc.to_le_bytes());
            return copy_body(&b, dest, capacity, out);
        }
        let s = h.summary.as_ref().ok_or("File has no summary")?;
        match op {
            1 | 2 | 5 | 8 => {
                let index = s
                    .chunk_indexes
                    .iter()
                    .find(|i| i.chunk_start_offset == offset)
                    .ok_or(mcap::McapError::BadIndex)?;
                if op == 8 {
                    out.value = index.compressed_data_offset()?;
                    return Ok(0);
                }
                if op == 1 {
                    let mut messages = VecDeque::new();
                    for m in s.stream_chunk(&h.data, index)? {
                        messages.push_back(own_message(m?));
                    }
                    h.messages = messages;
                    return Ok(0);
                }
                if op == 2 {
                    let m = s.seek_message(
                        &h.data,
                        index,
                        &records::MessageIndexEntry {
                            log_time: 0,
                            offset: argument,
                        },
                    )?;
                    let (msg, data) = own_message(m);
                    if !header.is_null() {
                        *header = msg;
                    }
                    return copy_body(&data, dest, capacity, out);
                }
                let indexes = s.read_message_indexes(&h.data, index)?;
                let mut rows: Vec<_> = indexes.into_iter().collect();
                rows.sort_by_key(|(c, _)| c.id);
                let mut b = Vec::new();
                for (c, entries) in rows {
                    for e in entries {
                        b.extend_from_slice(&c.id.to_le_bytes());
                        b.extend_from_slice(&e.log_time.to_le_bytes());
                        b.extend_from_slice(&e.offset.to_le_bytes());
                    }
                }
                copy_body(&b, dest, capacity, out)
            }
            3 => {
                let index = s
                    .metadata_indexes
                    .iter()
                    .find(|i| i.offset == offset)
                    .ok_or(mcap::McapError::BadIndex)?;
                mcap::read::metadata(&h.data, index)?;
                copy_body(
                    record_body(&h.data, offset, records::op::METADATA)?,
                    dest,
                    capacity,
                    out,
                )
            }
            4 => {
                let index = s
                    .attachment_indexes
                    .iter()
                    .find(|i| i.offset == offset)
                    .ok_or(mcap::McapError::BadIndex)?;
                mcap::read::attachment(&h.data, index)?;
                copy_body(
                    record_body(&h.data, offset, records::op::ATTACHMENT)?,
                    dest,
                    capacity,
                    out,
                )
            }
            _ => Err("Unknown snapshot operation".into()),
        }
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_free(p: *mut Snapshot) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_parse_record(
    op: u8,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        mcap::parse_record(op, bytes(p, n)?)?;
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_reader_record_into(
    p: *mut Reader,
    offset: u64,
    dest: *mut u8,
    capacity: usize,
    opcode: *mut u8,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let r = p.as_mut().ok_or("Null reader")?;
        let (op, data) = r.record_at(offset)?;
        if !opcode.is_null() {
            *opcode = op;
        }
        copy_body(&data, dest, capacity, out)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_prepare(
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    handle: *mut *mut PreparedChannel,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let schema = if v["schema"].is_null() {
            None
        } else {
            let s = &v["schema"];
            Some(Arc::new(mcap::Schema {
                id: number(s, "id")?.try_into()?,
                name: string(s, "name")?.into(),
                encoding: string(s, "encoding")?.into(),
                data: Cow::Owned(bytes(data, len)?.to_vec()),
            }))
        };
        *handle = Box::into_raw(Box::new(PreparedChannel(Arc::new(mcap::Channel {
            id: number(&v, "id")?.try_into()?,
            topic: string(&v, "topic")?.into(),
            message_encoding: string(&v, "encoding")?.into(),
            schema,
            metadata: map(&v["metadata"])?,
        }))));
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_free(p: *mut PreparedChannel) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_full_message(
    handle: *mut Writer,
    channel: *const PreparedChannel,
    h: *const MessageHeader,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let c = channel.as_ref().ok_or("Null channel")?;
        let h = h.as_ref().ok_or("Null header")?;
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write(&mcap::Message {
                channel: c.0.clone(),
                sequence: h.sequence,
                log_time: h.log_time,
                publish_time: h.publish_time,
                data: Cow::Borrowed(bytes(p, n)?),
            })?;
        Ok(0)
    });
    writer_result(handle, status)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_private(
    handle: *mut Writer,
    opcode: u8,
    chunks: bool,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let opts = if chunks {
            enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
        } else {
            enumset::EnumSet::new()
        };
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write_private_record(opcode, bytes(p, n)?, opts)?;
        Ok(0)
    });
    writer_result(handle, status)
}

const _: () = assert!(std::mem::size_of::<Event>() == 56);
const _: () = assert!(std::mem::offset_of!(Event, header) == 32);
