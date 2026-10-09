//! Batched ABI operations. Payloads are consumed synchronously.
use super::reader::{fm_reader_next, fm_reader_owned};
use super::writer::{Writer, SafeRejection, writer_guard, writer_result};
use super::{buffer_reader, lease, memory, bytes, guard, Error, Response, MessageHeader};
use std::{ptr, slice};
use mcap::records;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Range {
    pub offset: u32,
    pub length: u32,
}
#[repr(C)]
#[derive(Default)]
pub struct Progress {
    pub count: u64,
    pub bytes: u64,
    pub required: u64,
    pub scanned: u64,
    pub partial_validation: u32,
    pub reserved: u32,
}
const _: () = assert!(std::mem::size_of::<Range>() == 8);
const _: () = assert!(std::mem::size_of::<Progress>() == 40);

#[no_mangle]
pub unsafe extern "C" fn fm_writer_batch(
    handle: *mut Writer,
    headers: *const MessageHeader,
    ranges: *const Range,
    count: usize,
    data: *const u8,
    length: usize,
    completed: *mut usize,
    out: *mut Response,
) -> i32 {
    if !completed.is_null() {
        *completed = 0;
    }
    let status = writer_guard(out, |_| {
        let completed = completed.as_mut().ok_or("Null completion count")?;
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let inner = w.inner.as_mut().ok_or("Writer completed")?;
        let data = bytes(data, length)?;
        if count > isize::MAX as usize / std::mem::size_of::<MessageHeader>()
            || (count != 0 && (headers.is_null() || ranges.is_null()))
        {
            return Err("Invalid batch descriptors".into());
        }
        let headers = if count == 0 {
            &[]
        } else {
            slice::from_raw_parts(headers, count)
        };
        let ranges = if count == 0 {
            &[]
        } else {
            slice::from_raw_parts(ranges, count)
        };
        // No writer state has changed when any preflight check rejects the batch.
        for (h, r) in headers.iter().zip(ranges) {
            if (r.offset as usize)
                .checked_add(r.length as usize)
                .is_none_or(|end| end > length)
            {
                return Err("Batch payload range exceeds storage".into());
            }
            if !inner.contains_channel(h.channel_id) {
                let e = mcap::McapError::UnknownChannel(h.sequence, h.channel_id);
                return Err(if w.recoverable_errors & 16 != 0 {
                    Box::new(SafeRejection(e)) as Error
                } else {
                    Box::new(e)
                });
            }
        }
        for (h, r) in headers.iter().zip(ranges) {
            inner.write_to_known_channel(
                &records::MessageHeader {
                    channel_id: h.channel_id,
                    sequence: h.sequence,
                    log_time: h.log_time,
                    publish_time: h.publish_time,
                },
                &data[r.offset as usize..r.offset as usize + r.length as usize],
            )?;
            *completed += 1;
        }
        Ok(crate::protocol::status::SUCCESS)
    });
    writer_result(handle, status)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_lease_batch(
    handle: *mut Writer,
    batch: *const lease::Batch,
    headers: *const MessageHeader,
    header_count: usize,
    completed: *mut usize,
    out: *mut Response,
) -> i32 {
    if !completed.is_null() {
        *completed = 0;
    }
    let status = writer_guard(out, |_| {
        let completed = completed.as_mut().ok_or("Null completion count")?;
        let batch = batch.as_ref().ok_or("Null lease")?;
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let inner = w.inner.as_mut().ok_or("Writer completed")?;
        let headers = if headers.is_null() && header_count == 0 {
            None
        } else {
            if headers.is_null()
                || header_count != batch.messages.len()
                || header_count > isize::MAX as usize / std::mem::size_of::<MessageHeader>()
            {
                return Err("Invalid lease batch headers".into());
            }
            Some(slice::from_raw_parts(headers, header_count))
        };
        // Complete channel validation precedes all writes, including when using replacement headers.
        for (i, message) in batch.messages.iter().enumerate() {
            let header = headers.map_or(&message.header, |h| &h[i]);
            if !inner.contains_channel(header.channel_id) {
                let error = mcap::McapError::UnknownChannel(header.sequence, header.channel_id);
                return Err(if w.recoverable_errors & 16 != 0 {
                    Box::new(SafeRejection(error)) as Error
                } else {
                    Box::new(error) as Error
                });
            }
        }
        for (i, message) in batch.messages.iter().enumerate() {
            let header = headers.map_or(&message.header, |h| &h[i]);
            inner.write_to_known_channel(
                &records::MessageHeader {
                    channel_id: header.channel_id,
                    sequence: header.sequence,
                    log_time: header.log_time,
                    publish_time: header.publish_time,
                },
                message.data.as_ref(),
            )?;
            *completed += 1;
        }
        Ok(crate::protocol::status::SUCCESS)
    });
    writer_result(handle, status)
}

unsafe fn next(
    kind: u32,
    handle: *mut std::ffi::c_void,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    if kind == crate::protocol::reader_kind::SESSION {
        fm_reader_next(handle.cast(), dest, capacity, header, ptr::null_mut(), out)
    } else {
        buffer_reader::fm_buffer_reader_message(handle.cast(), dest, capacity, header, out)
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_read_batch(
    kind: u32,
    handle: *mut std::ffi::c_void,
    headers: *mut MessageHeader,
    ranges: *mut Range,
    count: usize,
    dest: *mut u8,
    capacity: usize,
    progress: *mut Progress,
    out: *mut Response,
) -> i32 {
    if progress.is_null() || out.is_null() {
        return crate::protocol::status::ERROR;
    }
    *progress = Progress::default();
    if handle.is_null()
        || kind > crate::protocol::reader_kind::BUFFER
        || count > isize::MAX as usize / std::mem::size_of::<MessageHeader>()
        || (capacity != 0 && dest.is_null())
        || (count != 0 && (headers.is_null() || ranges.is_null()))
        || capacity > u32::MAX as usize
    {
        return guard(out, |_| Err("Invalid read batch".into()));
    }
    *out = Response::default();
    for i in 0..count {
        let mut header = MessageHeader::default();
        let used = (*progress).bytes as usize;
        let target = if dest.is_null() { dest } else { dest.add(used) };
        let status = next(kind, handle, target, capacity - used, &mut header, out);
        if status != crate::protocol::status::SUCCESS {
            if status == crate::protocol::status::END {
                (*progress).scanned = (*out).value;
                (*progress).partial_validation = header.reserved as u32;
            }
            if status == crate::protocol::status::BUFFER_TOO_SMALL {
                (*progress).required = (*out).value;
            }
            return status;
        }
        *headers.add(i) = header;
        *ranges.add(i) = Range {
            offset: used as u32,
            length: (*out).value as u32,
        };
        (*progress).count += 1;
        (*progress).bytes += (*out).value;
    }
    crate::protocol::status::SUCCESS
}

struct VisitContext {
    sink: memory::Sink,
    stopped: bool,
}
unsafe extern "C" fn accept(
    context: *mut std::ffi::c_void,
    opcode: u8,
    header: *const MessageHeader,
    data: *const u8,
    length: usize,
    copied: *mut usize,
) -> i32 {
    let c = &mut *context.cast::<VisitContext>();
    let result = (c.sink.accept)(c.sink.context, opcode, header, data, length, copied);
    if result == crate::protocol::callback_status::STOP {
        c.stopped = true;
        crate::protocol::callback_status::ACCEPTED
    } else {
        result
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_visit_messages(
    kind: u32,
    handle: *mut std::ffi::c_void,
    sink: memory::Sink,
    count: usize,
    progress: *mut Progress,
    out: *mut Response,
) -> i32 {
    if progress.is_null() || out.is_null() {
        return crate::protocol::status::ERROR;
    }
    *progress = Progress::default();
    *out = Response::default();
    if kind > crate::protocol::reader_kind::BUFFER {
        return guard(out, |_| Err("Invalid reader kind".into()));
    }
    let mut context = VisitContext {
        sink,
        stopped: false,
    };
    let sink = memory::Sink {
        context: (&mut context as *mut VisitContext).cast(),
        accept,
    };
    for _ in 0..count {
        let mut header = MessageHeader::default();
        let status = if kind == crate::protocol::reader_kind::SESSION {
            fm_reader_owned(handle.cast(), 5, sink, &mut header, out)
        } else {
            buffer_reader::fm_buffer_reader_owned(handle.cast(), true, sink, out)
        };
        if status != crate::protocol::status::SUCCESS {
            if status == crate::protocol::status::END {
                (*progress).scanned = (*out).value;
                (*progress).partial_validation = header.reserved as u32;
            }
            return status;
        }
        (*progress).count += 1;
        if context.stopped {
            return crate::protocol::batch_status::VISITOR_STOPPED;
        }
    }
    crate::protocol::status::SUCCESS
}
