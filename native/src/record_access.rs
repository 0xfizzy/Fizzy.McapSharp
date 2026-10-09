use super::io::Input;
use super::reader::Reader;
use super::{buffer_reader, bytes, guard, Outcome, Response};
use mcap::records;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::ptr;

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
        Ok(crate::protocol::status::SUCCESS)
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
        return Ok(crate::protocol::status::BUFFER_TOO_SMALL);
    }
    if !data.is_empty() {
        if dest.is_null() {
            return Err("Null destination".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
    }
    Ok(crate::protocol::status::SUCCESS)
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
pub unsafe extern "C" fn fm_parse_record(
    op: u8,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        mcap::parse_record(op, bytes(p, n)?)?;
        Ok(crate::protocol::status::SUCCESS)
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
        super::restored(result, restored)
    })
}
