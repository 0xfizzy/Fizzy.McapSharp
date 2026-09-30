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
    engine: Engine,
    event: Event,
    pub delivery: memory::Delivery,
    waiting: bool,
    failed: bool,
    ended: bool,
    summary: Option<Arc<mcap::Summary>>,
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
            delivery: memory::Delivery {
                options: if kind == 2 && v["Memory"].is_null() {
                    summary.as_ref().ok_or("Missing summary")?.delivery.options
                } else {
                    memory::Options::parse(&v["Memory"])?
                },
                ..Default::default()
            },
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
                Engine::Linear(r) => match r.next_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::LinearReadEvent::ReadRequest(n)) => {
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::LinearReadEvent::Record { opcode, data }) => {
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
        h.delivery.stats.copied += n as u64;
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
    let status = writer_guard(out, |out| {
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
    pub memory_peak: u64,
    pub retry: random_access::Retry,
    pub cache: random_access::ChunkCache,
    pub stats: memory::Statistics,
    pub data: Arc<memory::Backing>,
    pub options: memory::Options,
    summary: Option<Arc<mcap::Summary>>,
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
    fm_snapshot_bytes_options(data, n, ptr::null(), 0, handle, out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_bytes_options(
    data: *const u8,
    n: usize,
    config: *const u8,
    config_len: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let options = if config_len == 0 {
            memory::Options::default()
        } else {
            memory::Options::parse(&request(config, config_len)?)?
        };
        let data = memory::Backing::copy(bytes(data, n)?, options)?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            memory_peak: 0,
            retry: Default::default(),
            cache: Default::default(),
            stats: memory::Statistics::default(),
            data: Arc::new(data),
            options,
            summary: summary.map(Arc::new),
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
            0 => (*(p as *const Snapshot)).summary.as_ref(),
            1 => (*(p as *const EngineHandle)).summary.as_ref(),
            2 => (*(p as *const Writer)).native_summary.as_ref(),
            _ => return Err("Unknown summary source".into()),
        };
        let mut cursor =
            buffer_reader::summary_records(summary.ok_or("No summary available")?.clone())?;
        cursor.delivery.options = match kind {
            0 => (*(p as *const Snapshot)).options,
            1 => (*(p as *const EngineHandle)).delivery.options,
            _ => memory::Options::default(),
        };
        *handle = Box::into_raw(Box::new(cursor));
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
    fm_snapshot_open_options(reader, ptr::null(), 0, handle, out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_open_options(
    reader: *mut Reader,
    config: *const u8,
    config_len: usize,
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
        let options = if config_len == 0 {
            r.delivery.options
        } else {
            memory::Options::parse(&request(config, config_len)?)?
        };
        let pos = r.input.stream_position()?;
        let result = (|| -> Outcome<Vec<u8>> {
            r.input.seek(SeekFrom::Start(0))?;
            let length = usize::try_from(r.input.seek(SeekFrom::End(0))?)?;
            memory::check("OwnedInput", options.owned, length)?;
            r.input.seek(SeekFrom::Start(0))?;
            let mut data = Vec::new();
            data.try_reserve_exact(length)?;
            memory::check("OwnedInput", options.owned, data.capacity())?;
            data.resize(length, 0);
            r.input.read_exact(&mut data)?;
            Ok(data)
        })();
        r.input.seek(SeekFrom::Start(pos))?;
        let data = result?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            memory_peak: 0,
            retry: Default::default(),
            cache: Default::default(),
            stats: memory::Statistics::default(),
            data: Arc::new(memory::Backing::Owned(data)),
            options,
            summary: summary.map(Arc::new),
        }));
        Ok(0)
    })
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
    index_data: *const u8,
    index_length: usize,
    message_time: u64,
    message_offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |out| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        if !header.is_null() {
            *header = MessageHeader::default();
        }
        h.stats.peak = h.stats.current;
        h.cache.stats.peak = h.cache.stats.current;
        let current = h.stats.current + h.cache.stats.current;
        h.memory_peak = h.memory_peak.max(current);
        let key = bytes(index_data, index_length)?;
        if let Some(status) = h.retry.read(op, key, message_time, message_offset,
            dest, capacity, header, out, &mut h.stats)? { return Ok(status); }
        h.stats.peak = h.stats.current;
        if op == 6 {
            let f = mcap::read::footer(&h.data)?;
            let mut b = [0u8; 20];
            b[..8].copy_from_slice(&f.summary_start.to_le_bytes());
            b[8..16].copy_from_slice(&f.summary_offset_start.to_le_bytes());
            b[16..].copy_from_slice(&f.summary_crc.to_le_bytes());
            return copy_body(&b, dest, capacity, out);
        }
        match op {
            2 | 5 | 8 => {
                let records::Record::ChunkIndex(index) =
                    mcap::parse_record(records::op::CHUNK_INDEX, bytes(index_data, index_length)?)?
                else {
                    unreachable!()
                };
                if op == 8 {
                    out.value = index.compressed_data_offset()?;
                    return Ok(0);
                }
                let s = h.summary.as_ref().ok_or("File has no summary")?;
                if op == 2 {
                    check_index_range(&h.data, index.chunk_start_offset, index.chunk_length, 9)?;
                }
                if op == 2 {
                    let cached = h.cache.read(&h.data, s, &index, key, message_offset,
                        h.options.random, dest, capacity, header, out);
                    match cached {
                        Ok(Some(status)) => {
                            // Common epilogue counts caller delivery once.
                            if status == 0 { h.cache.stats.copied -= out.value; }
                            return Ok(status);
                        }
                        Ok(None) => {},
                        Err(e) => { h.cache.clear(); return Err(e); }
                    }
                    let m = s.seek_message(
                        &h.data,
                        &index,
                        &records::MessageIndexEntry {
                            log_time: message_time,
                            offset: message_offset,
                        },
                    )?;
                    let msg = MessageHeader {
                        channel_id: m.channel.id,
                        sequence: m.sequence,
                        log_time: m.log_time,
                        publish_time: m.publish_time,
                        reserved: 0,
                    };
                    if !header.is_null() {
                        *header = msg;
                    }
                    let status = copy_body(&m.data, dest, capacity, out)?;
                    if status == 2 {
                        h.retry.save(op, key, message_time, message_offset, msg,
                            m.data.into_owned(), h.options, &mut h.stats);
                    }
                    return Ok(status);
                }
                for offset in index.message_index_offsets.values() {
                    check_index_range(&h.data, *offset, 15, 15)?;
                }
                let indexes = s.read_message_indexes(&h.data, &index)?;
                let mut rows: Vec<_> = indexes.into_iter().collect();
                rows.sort_by_key(|(c, _)| c.id);
                let length = rows
                    .iter()
                    .try_fold(0usize, |total, (_, entries)| {
                        entries
                            .len()
                            .checked_mul(18)
                            .and_then(|n| total.checked_add(n))
                    })
                    .ok_or("Message index length overflow")?;
                out.value = length as u64;
                if capacity < length {
                    let limit = h.options.pending.unwrap_or(u64::MAX).min(h.options.retained);
                    if length.checked_add(key.len()).is_some_and(|n| n as u64 <= limit) {
                        let mut packed = Vec::new();
                        if packed.try_reserve_exact(length).is_ok() {
                            for (c, entries) in rows {
                                for e in entries {
                                    packed.extend_from_slice(&c.id.to_le_bytes());
                                    packed.extend_from_slice(&e.log_time.to_le_bytes());
                                    packed.extend_from_slice(&e.offset.to_le_bytes());
                                }
                            }
                            h.stats.copied += length as u64;
                            h.retry.save(op, key, message_time, message_offset,
                                MessageHeader::default(), packed, h.options, &mut h.stats);
                        }
                    }
                    return Ok(2);
                }
                if length != 0 && dest.is_null() {
                    return Err("Null destination".into());
                }
                let mut offset = 0;
                for (c, entries) in rows {
                    for e in entries {
                        memory::copy(&c.id.to_le_bytes(), dest.add(offset))?;
                        memory::copy(&e.log_time.to_le_bytes(), dest.add(offset + 2))?;
                        memory::copy(&e.offset.to_le_bytes(), dest.add(offset + 10))?;
                        offset += 18;
                    }
                }
                Ok(0)
            }
            3 => {
                let records::Record::MetadataIndex(index) = mcap::parse_record(
                    records::op::METADATA_INDEX,
                    bytes(index_data, index_length)?,
                )?
                else {
                    unreachable!()
                };
                check_index_range(&h.data, index.offset, index.length, 0)?;
                mcap::read::metadata(&h.data, &index)?;
                copy_body(
                    record_body(&h.data, index.offset, records::op::METADATA)?,
                    dest,
                    capacity,
                    out,
                )
            }
            4 => {
                let records::Record::AttachmentIndex(index) = mcap::parse_record(
                    records::op::ATTACHMENT_INDEX,
                    bytes(index_data, index_length)?,
                )?
                else {
                    unreachable!()
                };
                check_index_range(&h.data, index.offset, index.length, 0)?;
                mcap::read::attachment(&h.data, &index)?;
                copy_body(
                    record_body(&h.data, index.offset, records::op::ATTACHMENT)?,
                    dest,
                    capacity,
                    out,
                )
            }
            _ => Err("Unknown snapshot operation".into()),
        }
    });
    if let Some(h) = p.as_mut() {
        if status == 0 && matches!(op, 2 | 3 | 4 | 5 | 6) && !out.is_null() {
            h.stats.copied += (*out).value;
        }
        h.memory_peak = h.memory_peak
            .max(h.stats.peak + h.cache.stats.current)
            .max(h.cache.stats.peak + h.stats.current);
    }
    status
}
// Reject overflowing/out-of-bounds caller indexes before upstream slice arithmetic.
fn check_index_range(data: &[u8], offset: u64, length: u64, minimum: u64) -> Outcome<()> {
    if length < minimum
        || offset
            .checked_add(length)
            .is_none_or(|end| end > data.len() as u64)
    {
        return Err(mcap::McapError::BadIndex.into());
    }
    Ok(())
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
        if r.failed { return Err("Reader failed".into()); }
        let pos = r.input.stream_position()?;
        let result = (|| {
            r.input.seek(SeekFrom::Start(offset))?;
            let mut h = [0u8; 9];
            r.input.read_exact(&mut h)?;
            let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
            if r.limit.is_some_and(|limit| n > limit) {
                return Err(mcap::McapError::RecordTooLarge { opcode: h[0], len: n as u64 }.into());
            }
            let start = r.input.stream_position()?;
            let end = r.input.seek(SeekFrom::End(0))?;
            if n as u64 > end.saturating_sub(start) { return Err("Record exceeds source length".into()); }
            let status = if let Input::Map { mapping, .. } = &r.input {
                let start = usize::try_from(start)?;
                let data = &mapping[start..start + n];
                mcap::parse_record(h[0], data)?;
                copy_body(data, dest, capacity, out)?
            } else {
                r.input.seek(SeekFrom::Start(start))?;
                r.scratch.data.clear();
                r.scratch.reserve_scratch(n)?;
                r.update_memory_peak();
                r.scratch.data.resize(n, 0);
                r.input.read_exact(&mut r.scratch.data)?;
                r.scratch.stats.copied += n as u64;
                mcap::parse_record(h[0], &r.scratch.data)?;
                copy_body(&r.scratch.data, dest, capacity, out)?
            };
            if status == 0 { r.scratch.stats.copied += n as u64; }
            if !opcode.is_null() { *opcode = h[0]; }
            Ok(status)
        })();
        let restored = r.input.seek(SeekFrom::Start(pos));
        r.update_memory_peak();
        r.scratch.release();
        // Budget rejection is terminal; other random-access errors retain existing semantics.
        if result.as_ref().err().is_some_and(|e: &Error| e.downcast_ref::<memory::Limit>().is_some()) {
            r.failed = true; r.parser = None; r.indexed = None;
            r.delivery.discard(); r.scratch.discard(); r.arena.clear();
        }
        restored?;
        result
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

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_chunk_reader(
    p: *const Snapshot,
    index_data: *const u8,
    index_length: usize,
    handle: *mut *mut buffer_reader::BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let h = p.as_ref().ok_or("Null snapshot")?;
        let records::Record::ChunkIndex(index) =
            mcap::parse_record(records::op::CHUNK_INDEX, bytes(index_data, index_length)?)?
        else {
            unreachable!()
        };
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let mut cursor = buffer_reader::chunk_reader(h.data.clone(), summary.clone(), &index)?;
        cursor.delivery.options = h.options;
        *handle = Box::into_raw(Box::new(cursor));
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_mapped(
    config: *const u8,
    n: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(config, n)?;
        let options = memory::Options::parse(&v["options"])?;
        let data = memory::Backing::open(string(&v, "path")?)?;
        let summary = mcap::Summary::read(&data)?.map(Arc::new);
        *handle = Box::into_raw(Box::new(Snapshot {
            memory_peak: 0,
            retry: Default::default(),
            cache: Default::default(),
            stats: memory::Statistics::default(),
            data: Arc::new(data),
            options,
            summary,
        }));
        Ok(0)
    })
}
