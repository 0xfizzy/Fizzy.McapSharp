use super::*;
use mcap::storage::SharedBytes;
pub struct Message {
    pub header: MessageHeader,
    pub data: SharedBytes,
}
pub struct Batch {
    published: bool,
    pub messages: mcap::segmented::BudgetedSegmentedVec<Message>,
    domain: std::sync::Arc<budget::MemoryBudget>,
}
impl Batch {
    pub fn new(domain: std::sync::Arc<budget::MemoryBudget>, count: usize) -> Outcome<Self> {
        let mut messages = mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
            domain.clone(),
            mcap::storage::ResourceCategory::Descriptor,
            count,
        );
        messages.reserve(count)?;
        Ok(Self {
            published: false,
            messages,
            domain,
        })
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        if self.published {
            for message in self.messages.iter() {
                message.data.lease_reference(false);
            }
            self.domain.release_lease();
        }
    }
}
pub fn publish(mut batch: Batch) -> *mut Batch {
    for message in batch.messages.iter() {
        message.data.lease_reference(true);
    }
    batch.domain.acquire_lease();
    batch.published = true;
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
            || kind > 1
            || count == 0
            || count > 65536
            || target == 0
        {
            return Err("Invalid lease request".into());
        }
        *progress = batch::Progress::default();
        let domain = if kind == 0 {
            (*handle.cast::<Reader>()).delivery.options.domain.clone()
        } else {
            (*handle.cast::<buffer_reader::BufferReader>())
                .delivery
                .options
                .domain
                .clone()
        };
        let mut batch = match Batch::new(domain.clone(), count) {
            Err(ref e) if budget::unavailable(e) && domain.has_leases() => return Ok(4),
            other => other?,
        };
        for _ in 0..count {
            let mut header = MessageHeader::default();
            let status = if kind == 0 {
                let r = &mut *handle.cast::<Reader>();
                r.delivery.capture = true;
                fm_reader_next(r, ptr::null_mut(), 0, &mut header, ptr::null_mut(), out)
            } else {
                let r = &mut *handle.cast::<buffer_reader::BufferReader>();
                r.delivery.capture = true;
                buffer_reader::fm_buffer_reader_message(r, ptr::null_mut(), 0, &mut header, out)
            };
            if status < 0 {
                return Ok(status);
            }
            if status == 4 {
                if !batch.messages.is_empty() {
                    *lease = publish(batch);
                    return Ok(0);
                }
                if !batch.domain.has_leases() {
                    return Err("Native budget cannot advance this reader without increasing its finite budget".into());
                }
                return Ok(4);
            }
            if status == 1 {
                (*progress).scanned = out.value;
                (*progress).partial_validation = header.reserved as u32;
                if !batch.messages.is_empty() {
                    *lease = publish(batch);
                }
                return Ok(1);
            }
            let delivery = if kind == 0 {
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
            batch.messages.push(Message { header, data })?;
            if (*progress).bytes >= target as u64 {
                break;
            }
        }
        *lease = publish(batch);
        Ok(0)
    });
    if !handle.is_null() {
        if kind == 0 {
            (*handle.cast::<Reader>()).delivery.capture = false;
        }
        if kind == 1 {
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
        Ok(0)
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
        let mut batch = Batch::new(owner.domain.clone(), 1)?;
        batch.messages.push(Message {
            header: m.header,
            data: m.data.clone(),
        })?;
        *output = publish(batch);
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_lease_free(lease: *mut Batch) {
    if !lease.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(lease))));
    }
}
