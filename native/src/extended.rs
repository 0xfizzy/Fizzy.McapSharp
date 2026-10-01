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
    lease_batch: Option<lease::Batch>,
    schemas: BTreeMap<u16, Arc<mcap::Schema<'static>>>,
    channels: BTreeMap<u16, Arc<mcap::Channel<'static>>>,
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
    pub cache: chunk_cache::ChunkCache,
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
        let data = memory::Backing::copy(bytes(data, n)?)?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            cache: chunk_cache::ChunkCache::new(),
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
        let cursor =
            buffer_reader::summary_records(summary.ok_or("No summary available")?.clone())?;
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
            r.options.clone()
        } else {
            memory::Options::parse(&request(config, config_len)?)?
        };
        let pos = r.input.stream_position()?;
        let result = (|| -> Outcome<memory::Backing> {
            r.input.seek(SeekFrom::Start(0))?;
            let length = usize::try_from(r.input.seek(SeekFrom::End(0))?)?;
            r.input.seek(SeekFrom::Start(0))?;
            let mut data = Vec::new();
            data.try_reserve_exact(length)?;
            data.resize(length, 0);
            r.input.read_exact(&mut data)?;
            Ok(memory::Backing::Owned { data })
        })();
        r.input.seek(SeekFrom::Start(pos))?;
        let data = result?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            cache: chunk_cache::ChunkCache::new(),
            data: Arc::new(data),
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
pub(super) fn record_body(data: &[u8], offset: u64, expected: u8) -> Outcome<&[u8]> {
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
    _message_time: u64,
    message_offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    snapshot_call(
        p,
        op,
        index_data,
        index_length,
        _message_time,
        message_offset,
        dest,
        capacity,
        header,
        out,
        None,
    )
}
unsafe fn snapshot_call(
    p: *mut Snapshot,
    op: u32,
    index_data: *const u8,
    index_length: usize,
    _message_time: u64,
    message_offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
    prepared: Option<&PreparedChunkIndex>,
) -> i32 {
    let status = guard(out, |out| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        if !header.is_null() {
            *header = MessageHeader::default();
        }
        let key = bytes(index_data, index_length)?;
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
                let parsed;
                let index = if let Some(prepared) = prepared {
                    &prepared.index
                } else {
                    parsed = parse_chunk_index(bytes(index_data, index_length)?)?;
                    &parsed
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
                    let cached = h.cache.read(
                        &h.data,
                        s,
                        &index,
                        key,
                        message_offset,
                        h.options.random,
                        dest,
                        capacity,
                        header,
                        out,
                    );
                    match cached {
                        Ok(Some(status)) => {
                            // Common epilogue counts caller delivery once.
                            return Ok(status);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            h.cache.clear();
                            return Err(e);
                        }
                    }
                    return Err("Chunk cache did not produce a result".into());
                }
                for offset in index.message_index_offsets.values() {
                    check_index_range(&h.data, *offset, 15, 15)?;
                }
                let packed = h
                    .cache
                    .message_indexes(&h.data, s, index, key, h.options.random)?;
                copy_body(&packed.data, dest, capacity, out)
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
        if r.failed {
            return Err("Reader failed".into());
        }
        let pos = r.input.stream_position()?;
        let result = (|| {
            r.input.seek(SeekFrom::Start(offset))?;
            let mut h = [0u8; 9];
            r.input.read_exact(&mut h)?;
            let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
            if r.limit.is_some_and(|limit| n > limit) {
                return Err(mcap::McapError::RecordTooLarge {
                    opcode: h[0],
                    len: n as u64,
                }
                .into());
            }
            let start = r.input.stream_position()?;
            let end = r.input.seek(SeekFrom::End(0))?;
            if n as u64 > end.saturating_sub(start) {
                return Err("Record exceeds source length".into());
            }
            let status = if let Input::Map { mapping, .. } = &r.input {
                let start = usize::try_from(start)?;
                let data = &mapping[start..start + n];
                mcap::parse_record(h[0], data)?;
                copy_body(data, dest, capacity, out)?
            } else {
                r.input.seek(SeekFrom::Start(start))?;
                r.scratch.data.clear();
                r.scratch.reserve_scratch(n)?;
                r.scratch.data.resize(n, 0);
                r.input.read_exact(&mut r.scratch.data)?;
                mcap::parse_record(h[0], &r.scratch.data)?;
                copy_body(&r.scratch.data, dest, capacity, out)?
            };
            if !opcode.is_null() {
                *opcode = h[0];
            }
            Ok(status)
        })();
        let restored = r.input.seek(SeekFrom::Start(pos));
        r.scratch.release();
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
        let cursor = buffer_reader::chunk_reader(h.data.clone(), summary.clone(), &index)?;
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
            cache: chunk_cache::ChunkCache::new(),
            data: Arc::new(data),
            options,
            summary,
        }));
        Ok(0)
    })
}

// Immutable owned index: no managed dictionaries or borrowed record memory survive preparation.
pub struct PreparedChunkIndex {
    index: records::ChunkIndex,
    key: Vec<u8>,
}
fn parse_chunk_index(data: &[u8]) -> Outcome<records::ChunkIndex> {
    let records::Record::ChunkIndex(index) = mcap::parse_record(records::op::CHUNK_INDEX, data)?
    else {
        unreachable!()
    };
    Ok(index)
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_prepare(
    data: *const u8,
    n: usize,
    handle: *mut *mut PreparedChunkIndex,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let bytes = bytes(data, n)?;
        let index = parse_chunk_index(bytes)?;
        *handle = Box::into_raw(Box::new(PreparedChunkIndex {
            index,
            key: bytes.to_vec(),
        }));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_free(p: *mut PreparedChunkIndex) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_prepared_call(
    p: *mut Snapshot,
    op: u32,
    index: *const PreparedChunkIndex,
    time: u64,
    offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    let Some(index) = index.as_ref() else {
        return guard(out, |_| Err("Null prepared index".into()));
    };
    snapshot_call(
        p,
        op,
        index.key.as_ptr(),
        index.key.len(),
        time,
        offset,
        dest,
        capacity,
        header,
        out,
        Some(index),
    )
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_prepared_chunk_reader(
    p: *const Snapshot,
    index: *const PreparedChunkIndex,
    handle: *mut *mut buffer_reader::BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let h = p.as_ref().ok_or("Null snapshot")?;
        let index = index.as_ref().ok_or("Null prepared index")?;
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let cursor = buffer_reader::chunk_reader(h.data.clone(), summary.clone(), &index.index)?;
        *handle = Box::into_raw(Box::new(cursor));
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_message_owned(
    p: *mut Snapshot,
    data: *const u8,
    n: usize,
    prepared: *const PreparedChunkIndex,
    _time: u64,
    offset: u64,
    sink: memory::Sink,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        let parsed;
        let (index, key) = if let Some(prepared) = prepared.as_ref() {
            (&prepared.index, prepared.key.as_slice())
        } else {
            let key = bytes(data, n)?;
            parsed = parse_chunk_index(key)?;
            (&parsed, key)
        };
        check_index_range(&h.data, index.chunk_start_offset, index.chunk_length, 9)?;
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let result = h.cache.read_with(
            &h.data,
            summary,
            index,
            key,
            offset,
            h.options.random,
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut Response::default(),
            Some(sink),
        );
        match result {
            Ok(Some(_)) => return Ok(0),
            Err(e) => {
                h.cache.clear();
                return Err(e);
            }
            Ok(None) => {}
        }
        Err("Chunk cache did not produce a result".into())
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

#[repr(C)]
pub struct SeekRequest {
    index: *const PreparedChunkIndex,
    time: u64,
    offset: u64,
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_seek_batch(
    p: *mut Snapshot,
    requests: *const SeekRequest,
    count: usize,
    output: *mut *mut lease::Batch,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let result = output.as_mut().ok_or("Null output")?;
        *result = ptr::null_mut();
        let h = p.as_mut().ok_or("Null snapshot")?;
        if count == 0 || count > 65536 || requests.is_null() {
            return Err("Invalid seek batch".into());
        }
        let requests = slice::from_raw_parts(requests, count);
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        // Validate all descriptor handles before loading anything.
        for req in requests {
            if req.index.is_null() {
                return Err("Null prepared index".into());
            }
        }
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_unstable_by(|a, b| (*requests[*a].index).key.cmp(&(*requests[*b].index).key));
        let mut batch = lease::Batch::new(count)?;
        let mut previous: Option<&[u8]> = None;
        let mut chunk = None;
        // Output slots are filled in request order after sorting compact descriptors.
        let mut slots: Vec<Option<lease::Message>> =
            std::iter::repeat_with(|| None).take(count).collect();
        for i in order {
            let req = &requests[i];
            let index = &*req.index;
            if previous != Some(index.key.as_slice()) {
                chunk = Some(
                    h.cache
                        .load(&h.data, &index.index, &index.key, h.options.random)?,
                );
                previous = Some(&index.key);
            }
            slots[i] = Some(chunk.as_ref().unwrap().message(summary, req.offset)?);
        }
        batch.messages.extend(slots.into_iter().map(Option::unwrap));
        *result = lease::publish(batch);
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_cache_statistics(
    p: *const Snapshot,
    hits: *mut u64,
    loads: *mut u64,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        *hits.as_mut().ok_or("Null hits")? = h.cache.hits;
        *loads.as_mut().ok_or("Null loads")? = h.cache.loads;
        Ok(0)
    })
}
