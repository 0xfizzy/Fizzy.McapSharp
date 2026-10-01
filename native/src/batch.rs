//! Batched ABI operations. Payloads are consumed synchronously.
use super::*;

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
                    Error::safe_rejection(e)
                } else {
                    Error::from(e)
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
        Ok(0)
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
    if kind == 0 {
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
        return -1;
    }
    *progress = Progress::default();
    if handle.is_null()
        || kind > 1
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
        if status != 0 {
            if status == 1 {
                (*progress).scanned = (*out).value;
                (*progress).partial_validation = header.reserved as u32;
            }
            if status == 2 {
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
    0
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
    if result == 1 {
        c.stopped = true;
        0
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
        return -1;
    }
    *progress = Progress::default();
    *out = Response::default();
    if kind > 1 {
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
        let status = if kind == 0 {
            fm_reader_owned(handle.cast(), 5, sink, &mut header, out)
        } else {
            buffer_reader::fm_buffer_reader_owned(handle.cast(), true, sink, out)
        };
        if status != 0 {
            if status == 1 {
                (*progress).scanned = (*out).value;
                (*progress).partial_validation = header.reserved as u32;
            }
            return status;
        }
        (*progress).count += 1;
        if context.stopped {
            return 3;
        }
    }
    0
}
