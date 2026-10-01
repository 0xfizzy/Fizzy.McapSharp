use super::*;
use mcap::charged::ChargedBox;
use mcap::storage::{OwnerKind, ResourceCategory, SharedBytes};
pub type Handle = std::ffi::c_void;
pub struct Message {
    pub header: MessageHeader,
    pub data: SharedBytes,
}
pub struct Batch {
    pub message_limit: usize,
    published: bool,
    pub messages: mcap::segmented::BudgetedSegmentedVec<Message>,
    domain: mcap::storage::BudgetRef,
}
impl Batch {
    pub fn reservation_bytes(count: usize) -> std::io::Result<usize> {
        mcap::segmented::BudgetedSegmentedVec::<Message>::fresh_reservation_bytes(count,count)?
            .checked_add(ChargedBox::<Self>::allocation_size())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }
    pub fn new(
        domain: mcap::storage::BudgetRef,
        count: usize,
    ) -> Outcome<ChargedBox<Self>> {
        let mut messages = mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
            domain.clone(),
            mcap::storage::ResourceCategory::Descriptor,
            count,
        );
        messages.reserve(count)?;
        messages.owner_reference(OwnerKind::Operation, true);
        let batch = ChargedBox::new(
            Self {
                message_limit: count,
                published: false,
                messages,
                domain: domain.clone(),
            },
            &domain,
            ResourceCategory::Descriptor,
        )?;
        batch.charge_owner(OwnerKind::Operation, true);
        Ok(batch)
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.messages.owner_reference(
            if self.published {
                OwnerKind::Lease
            } else {
                OwnerKind::Operation
            },
            false,
        );
        if self.published {
            for message in self.messages.iter() {
                message.data.lease_reference(false);
            }
            self.domain.release_lease();
        }
    }
}
pub fn publish(mut batch: ChargedBox<Batch>) -> *mut Handle {
    batch.charge_owner(OwnerKind::Lease, true);
    batch.charge_owner(OwnerKind::Operation, false);
    batch
        .messages
        .owner_reference(mcap::storage::OwnerKind::Lease, true);
    batch.messages.owner_reference(OwnerKind::Operation, false);
    for i in 0..batch.messages.len() {
        batch.messages[i]
            .data
            .set_owner(mcap::storage::OwnerKind::Lease);
        batch.messages[i].data.lease_reference(true);
    }
    batch.domain.acquire_lease();
    batch.published = true;
    batch.into_handle().as_ptr().cast()
}

#[no_mangle]
pub unsafe extern "C" fn fm_read_lease(
    kind: u32,
    handle: *mut std::ffi::c_void,
    count: usize,
    target: usize,
    lease: *mut *mut Handle,
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
            Err(ref e) if budget::unavailable(e) => {
                domain.retry_status(Batch::reservation_bytes(count)?)?;
                return Ok(4);
            },
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
                let requested=if kind==0 {(*handle.cast::<Reader>()).delivery.retry_capacity}
                    else {(*handle.cast::<buffer_reader::BufferReader>()).delivery.retry_capacity};
                let required=requested.checked_add(batch.allocation_bytes()).and_then(|n|n.checked_add(batch.messages.allocated_bytes()))
                    .ok_or("Retry capacity overflow")?;
                drop(batch);
                domain.retry_status(required)?;
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
unsafe fn borrow<'a>(handle: *const Handle) -> Outcome<&'a Batch> {
    let pointer = std::ptr::NonNull::new(handle.cast_mut().cast::<()>()).ok_or("Null lease")?;
    Ok(ChargedBox::<Batch>::borrow_handle(pointer))
}
#[no_mangle]
pub unsafe extern "C" fn fm_lease_get(
    lease: *const Handle,
    index: usize,
    header: *mut MessageHeader,
    data: *mut *const u8,
    length: *mut usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let owner = borrow(lease)?;
        let m = owner
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
    lease: *const Handle,
    index: usize,
    output: *mut *mut Handle,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let output = output.as_mut().ok_or("Null output")?;
        *output = ptr::null_mut();
        let owner = borrow(lease)?;
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
pub unsafe extern "C" fn fm_lease_free(lease: *mut Handle) {
    if !lease.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let batch=ChargedBox::<Batch>::from_handle(std::ptr::NonNull::new_unchecked(lease.cast()));
            let _release=batch.domain.begin_capacity_release();
            drop(batch);
        }));
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;
    #[test]
    fn root_refusal_rolls_back_all_descriptor_pages_and_owners() {
        let probe = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let batch = Batch::new(probe.clone(), 1).unwrap();
        let required = probe.workload_statistics().current as usize;
        drop(batch);
        assert_eq!(probe.ownership_statistics(), Default::default());
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            total: (required - 1) + mcap::storage::BudgetRef::allocation_size(),
            block: required,
            retained: 0,
        }).unwrap();
        assert!(Batch::new(domain.clone(), 1).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn published_root_and_descriptors_survive_original_owner_release() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let source = std::sync::Arc::new(vec![1, 2, 3]);
        let mut batch = Batch::new(domain.clone(), 1).unwrap();
        batch
            .messages
            .push(Message {
                header: MessageHeader::default(),
                data: SharedBytes::external(source, 0..3),
            })
            .unwrap();
        let capacity = domain.workload_statistics().current;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            capacity
        );
        let allocated = domain.workload_detailed_statistics().allocation_count;
        let handle = publish(batch);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, allocated);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Lease as usize],
            capacity
        );
        let mut retained = ptr::null_mut();
        let mut response = Response::default();
        assert_eq!(
            unsafe { fm_lease_retain(handle, 0, &mut retained, &mut response) },
            0
        );
        unsafe {
            fm_lease_free(handle);
        }
        assert_eq!(domain.lease_count(), 1);
        assert_eq!(
            unsafe { borrow(retained).unwrap().messages[0].data.as_ref() },
            &[1, 2, 3]
        );
        unsafe {
            fm_lease_free(retained);
        }
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        assert_eq!(domain.lease_count(), 0);
    }
}
