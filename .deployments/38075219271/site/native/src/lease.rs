#[cfg(test)]
use super::fm_buffer_free;
use super::reader::{Reader, fm_reader_next};
use super::{batch, buffer_reader, guard, Outcome, Response, MessageHeader};
use std::ptr;
use mcap::storage::SharedBytes;
pub struct Message {
    pub header: MessageHeader,
    pub data: SharedBytes,
}
pub struct Batch {
    pub messages: Vec<Message>,
}
impl Batch {
    pub fn new(count: usize) -> Outcome<Self> {
        let mut messages = Vec::new();
        messages.try_reserve_exact(count)?;
        Ok(Self { messages })
    }
}
pub fn publish(batch: Batch) -> *mut Batch {
    Box::into_raw(Box::new(batch))
}

#[no_mangle]
pub unsafe extern "C" fn fm_read_lease(
    kind: u32,
    handle: *mut std::ffi::c_void,
    count: usize,
    target: usize,
    lease: *mut *mut Batch,
    progress: *mut batch::Progress,
    out: *mut Response,
) -> i32 {
    if !lease.is_null() {
        *lease = ptr::null_mut();
    }
    let status = guard(out, |out| {
        if lease.is_null()
            || progress.is_null()
            || handle.is_null()
            || kind > crate::protocol::reader_kind::BUFFER
            || count == 0
            || count > 65536
            || target == 0
        {
            return Err("Invalid lease request".into());
        }
        *progress = batch::Progress::default();
        let mut batch = Batch::new(count)?;
        for _ in 0..count {
            let mut header = MessageHeader::default();
            let status = if kind == crate::protocol::reader_kind::SESSION {
                let r = &mut *handle.cast::<Reader>();
                r.delivery.capture = true;
                fm_reader_next(r, ptr::null_mut(), 0, &mut header, ptr::null_mut(), out)
            } else {
                let r = &mut *handle.cast::<buffer_reader::BufferReader>();
                r.delivery.capture = true;
                buffer_reader::fm_buffer_reader_message(r, ptr::null_mut(), 0, &mut header, out)
            };
            if status < crate::protocol::status::SUCCESS {
                return Ok(status);
            }
            if status == crate::protocol::status::END {
                (*progress).scanned = out.value;
                (*progress).partial_validation = header.reserved as u32;
                if !batch.messages.is_empty() {
                    *lease = publish(batch);
                }
                return Ok(crate::protocol::status::END);
            }
            let delivery = if kind == crate::protocol::reader_kind::SESSION {
                &mut (*handle.cast::<Reader>()).delivery
            } else {
                &mut (*handle.cast::<buffer_reader::BufferReader>()).delivery
            };
            let data = delivery
                .shared
                .take()
                .ok_or("Missing stable message storage")?;
            (*progress).count += 1;
            (*progress).bytes += data.as_ref().len() as u64;
            batch.messages.push(Message { header, data });
            if (*progress).bytes >= target as u64 {
                break;
            }
        }
        *lease = publish(batch);
        Ok(crate::protocol::status::SUCCESS)
    });
    if !handle.is_null() {
        if kind == crate::protocol::reader_kind::SESSION {
            (*handle.cast::<Reader>()).delivery.capture = false;
        }
        if kind == crate::protocol::reader_kind::BUFFER {
            (*handle.cast::<buffer_reader::BufferReader>())
                .delivery
                .capture = false;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_lease_get(
    lease: *const Batch,
    index: usize,
    header: *mut MessageHeader,
    data: *mut *const u8,
    length: *mut usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let m = lease
            .as_ref()
            .ok_or("Null lease")?
            .messages
            .get(index)
            .ok_or("Message index out of range")?;
        *header.as_mut().ok_or("Null header")? = m.header;
        *data.as_mut().ok_or("Null data")? = m.data.as_ref().as_ptr();
        *length.as_mut().ok_or("Null length")? = m.data.as_ref().len();
        Ok(crate::protocol::status::SUCCESS)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_lease_retain(
    lease: *const Batch,
    index: usize,
    output: *mut *mut Batch,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let output = output.as_mut().ok_or("Null output")?;
        *output = ptr::null_mut();
        let owner = lease.as_ref().ok_or("Null lease")?;
        let m = owner
            .messages
            .get(index)
            .ok_or("Message index out of range")?;
        let mut batch = Batch::new(1)?;
        batch.messages.push(Message {
            header: m.header,
            data: m.data.clone(),
        });
        *output = publish(batch);
        Ok(crate::protocol::status::SUCCESS)
    })
}
#[cfg(test)]
pub unsafe fn fm_lease_free(lease: *mut Batch) {
    let mut response = Response::default();
    fm_lease_release(lease, &mut response);
    fm_buffer_free(response.json, response.json_len);
    fm_buffer_free(response.data, response.data_len);
}

#[no_mangle]
pub unsafe extern "C" fn fm_lease_release(lease: *mut Batch, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !lease.is_null() { drop(Box::from_raw(lease)); }
        Ok(crate::protocol::status::SUCCESS)
    })
}
