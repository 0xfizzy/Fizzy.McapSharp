use super::reader::Reader;
use super::writer::Writer;
use super::summary::summary_json;
use super::engine::EngineHandle;
#[cfg(test)]
use super::fm_buffer_free;
use super::record_access::{copy_body, record_body};
use super::{
    buffer_reader, bytes, chunk_cache, guard, lease, memory, request, respond,
    string, MessageHeader, Outcome, Response,
};
use mcap::records;
use binrw::BinWrite;
use std::borrow::Cow;
use std::ptr;
use std::slice;
use std::sync::Arc;

pub struct Snapshot {
    cache: chunk_cache::ChunkCache,
    data: Arc<memory::Backing>,
    options: memory::Options,
    summary: Option<Arc<mcap::Summary>>,
}

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_summary(p: *const Snapshot, out: *mut Response) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        if let Some(s) = &h.summary {
            respond(out, serde_json::to_vec(&summary_json(s))?, vec![], 0);
        }
        Ok(crate::protocol::status::SUCCESS)
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
        Ok(crate::protocol::status::SUCCESS)
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
            crate::protocol::summary_source::SNAPSHOT => (*(p as *const Snapshot)).summary.as_ref(),
            crate::protocol::summary_source::ENGINE => (*(p as *const EngineHandle)).summary.as_ref(),
            crate::protocol::summary_source::WRITER => (*(p as *const Writer)).native_summary.as_ref(),
            _ => return Err("Unknown summary source".into()),
        };
        let cursor =
            buffer_reader::summary_records(summary.ok_or("No summary available")?.clone())?;
        *handle = Box::into_raw(Box::new(cursor));
        Ok(crate::protocol::status::SUCCESS)
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
        let options = if config_len == 0 {
            r.options.clone()
        } else {
            memory::Options::parse(&request(config, config_len)?)?
        };
        let data = r.copy_input()?;
        let summary = mcap::Summary::read(&data)?;
        *handle = Box::into_raw(Box::new(Snapshot {
            cache: chunk_cache::ChunkCache::new(),
            data: Arc::new(data),
            options,
            summary: summary.map(Arc::new),
        }));
        Ok(crate::protocol::status::SUCCESS)
    })
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
        if op == crate::protocol::snapshot_operation::FOOTER {
            let f = mcap::read::footer(&h.data)?;
            let mut b = [0u8; 20];
            b[..8].copy_from_slice(&f.summary_start.to_le_bytes());
            b[8..16].copy_from_slice(&f.summary_offset_start.to_le_bytes());
            b[16..].copy_from_slice(&f.summary_crc.to_le_bytes());
            return copy_body(&b, dest, capacity, out);
        }
        match op {
            crate::protocol::snapshot_operation::SEEK_MESSAGE | crate::protocol::snapshot_operation::MESSAGE_INDEXES | crate::protocol::snapshot_operation::COMPRESSED_DATA_OFFSET => {
                let parsed;
                let index = if let Some(prepared) = prepared {
                    &prepared.index
                } else {
                    parsed = parse_chunk_index(bytes(index_data, index_length)?)?;
                    &parsed
                };
                if op == crate::protocol::snapshot_operation::COMPRESSED_DATA_OFFSET {
                    out.value = index.compressed_data_offset()?;
                    return Ok(crate::protocol::status::SUCCESS);
                }
                let canonical_key = if prepared.is_some() {
                    Cow::Borrowed(key)
                } else {
                    canonical_chunk_key(key, index)?
                };
                let key = canonical_key.as_ref();
                let s = h.summary.as_ref().ok_or("File has no summary")?;
                if op == crate::protocol::snapshot_operation::SEEK_MESSAGE {
                    check_index_range(&h.data, index.chunk_start_offset, index.chunk_length, 9)?;
                }
                if op == crate::protocol::snapshot_operation::SEEK_MESSAGE {
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
            crate::protocol::snapshot_operation::METADATA | crate::protocol::snapshot_operation::ATTACHMENT => {
                let (_, body) = indexed_record_body(&h.data, op, bytes(index_data, index_length)?)?;
                copy_body(body, dest, capacity, out)
            }
            _ => Err("Unknown snapshot operation".into()),
        }
    });
    status
}

// Validate with the same upstream random-access APIs for both owned and caller-buffer delivery.
fn indexed_record_body<'a>(data: &'a [u8], op: u32, encoded: &[u8]) -> Outcome<(u8, &'a [u8])> {
    let (opcode, offset) = match op {
        crate::protocol::snapshot_operation::METADATA => {
            let records::Record::MetadataIndex(index) = mcap::parse_record(records::op::METADATA_INDEX, encoded)? else { unreachable!() };
            check_index_range(data, index.offset, index.length, 0)?;
            mcap::read::metadata(data, &index)?;
            (records::op::METADATA, index.offset)
        }
        crate::protocol::snapshot_operation::ATTACHMENT => {
            let records::Record::AttachmentIndex(index) = mcap::parse_record(records::op::ATTACHMENT_INDEX, encoded)? else { unreachable!() };
            check_index_range(data, index.offset, index.length, 0)?;
            mcap::read::attachment(data, &index)?;
            (records::op::ATTACHMENT, index.offset)
        }
        _ => return Err("Unknown indexed record operation".into()),
    };
    Ok((opcode, record_body(data, offset, opcode)?))
}

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_record_owned(
    p: *mut Snapshot,
    op: u32,
    index_data: *const u8,
    index_length: usize,
    sink: memory::Sink,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        let (opcode, body) = indexed_record_body(&h.data, op, bytes(index_data, index_length)?)?;
        out.value = sink.send(opcode, &MessageHeader::default(), body)?;
        Ok(crate::protocol::status::SUCCESS)
    })
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

#[cfg(test)]
pub unsafe fn fm_snapshot_free(p: *mut Snapshot) {
    let mut response = Response::default();
    fm_snapshot_release(p, &mut response);
    fm_buffer_free(response.json, response.json_len);
    fm_buffer_free(response.data, response.data_len);
}

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_release(p: *mut Snapshot, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(crate::protocol::status::SUCCESS)
    })
}

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
        Ok(crate::protocol::status::SUCCESS)
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
        Ok(crate::protocol::status::SUCCESS)
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

// Preserve the entire descriptor as the cache identity, but normalize map order.
// Already canonical managed input borrows its bytes; only reordered maps need a copy.
fn canonical_chunk_key<'a>(data: &'a [u8], index: &records::ChunkIndex) -> Outcome<Cow<'a, [u8]>> {
    let map_size = index.message_index_offsets.len() * 10;
    let canonical = data.get(36..36 + map_size).is_some_and(|map| {
        map.chunks_exact(10).zip(&index.message_index_offsets).all(|(entry, (id, offset))| {
            entry[..2] == id.to_le_bytes() && entry[2..] == offset.to_le_bytes()
        })
    });
    if canonical {
        return Ok(Cow::Borrowed(data));
    }
    let mut encoded = std::io::Cursor::new(Vec::with_capacity(data.len()));
    index.write_le(&mut encoded)?;
    Ok(Cow::Owned(encoded.into_inner()))
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
            key: canonical_chunk_key(bytes, &index)?.into_owned(),
            index,
        }));
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_release(
    p: *mut PreparedChunkIndex,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(crate::protocol::status::SUCCESS)
    })
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
        Ok(crate::protocol::status::SUCCESS)
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
        let canonical_key;
        let (index, key) = if let Some(prepared) = prepared.as_ref() {
            (&prepared.index, prepared.key.as_slice())
        } else {
            let key = bytes(data, n)?;
            parsed = parse_chunk_index(key)?;
            canonical_key = canonical_chunk_key(key, &parsed)?;
            (&parsed, canonical_key.as_ref())
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
            Ok(Some(_)) => return Ok(crate::protocol::status::SUCCESS),
            Err(e) => {
                h.cache.clear();
                return Err(e);
            }
            Ok(None) => {}
        }
        Err("Chunk cache did not produce a result".into())
    })
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
        Ok(crate::protocol::status::SUCCESS)
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
        Ok(crate::protocol::status::SUCCESS)
    })
}
