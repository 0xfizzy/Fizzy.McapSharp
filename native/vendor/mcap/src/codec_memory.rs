//! Private allocator callbacks for the pinned codec C APIs.
use crate::storage::{MemoryBudget, Reservation, ResourceCategory};
use std::{
    alloc::{alloc, dealloc, Layout},
    ffi::c_void,
    ptr,
    sync::{Arc, Mutex},
};

pub(crate) struct CodecMemory {
    budget: Arc<MemoryBudget>,
    category: ResourceCategory,
    failure: Mutex<Option<std::io::Error>>,
}
#[repr(C, align(16))]
struct Header {
    charge: Reservation,
    layout: Layout,
}
impl CodecMemory {
    pub fn new(budget: Arc<MemoryBudget>, category: ResourceCategory) -> Arc<Self> {
        Arc::new(Self {
            budget,
            category,
            failure: Mutex::new(None),
        })
    }
    pub fn opaque(this: &Arc<Self>) -> *mut c_void {
        Arc::as_ptr(this).cast_mut().cast()
    }
    pub fn progress(&self, input: usize, output: usize) {
        self.budget.codec_bytes(
            matches!(self.category, ResourceCategory::CodecEncoder),
            input,
            output,
        );
    }
    pub fn decode_event(&self, complete: bool) {
        self.budget.decode_event(complete);
    }
    pub fn error(&self) -> std::io::Error {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| std::io::Error::other("Codec allocation failed"))
    }
    pub fn take_error(&self) -> Option<std::io::Error> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
    fn allocate(&self, size: usize, zero: bool) -> std::io::Result<*mut c_void> {
        let total = size
            .max(1)
            .checked_add(std::mem::size_of::<Header>())
            .ok_or_else(|| std::io::Error::other("Codec allocation size overflow"))?;
        let layout = Layout::from_size_align(total, std::mem::align_of::<Header>())
            .map_err(std::io::Error::other)?;
        let mut charge = self
            .budget
            .reserve_class(total, self.category)
            // A codec may already have mutated internal state: never advertise retryability.
            .map_err(|e| {
                let message = e.to_string();
                std::io::Error::other(e.into_inner().unwrap_or_else(|| message.into()))
            })?;
        let p = unsafe { alloc(layout) };
        if p.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "Codec system allocation failed",
            ));
        }
        charge.commit(total);
        unsafe {
            p.cast::<Header>().write(Header { charge, layout });
            let data = p.add(std::mem::size_of::<Header>());
            if zero {
                data.write_bytes(0, size);
            }
            Ok(data.cast())
        }
    }
}
unsafe extern "C" fn allocate_impl(opaque: *mut c_void, size: usize, zero: bool) -> *mut c_void {
    if opaque.is_null() {
        return ptr::null_mut();
    }
    let state = &*opaque.cast::<CodecMemory>();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.allocate(size, zero))) {
        Ok(Ok(p)) => p,
        result => {
            let e = match result {
                Ok(Err(e)) => e,
                _ => std::io::Error::other("Codec allocator panic"),
            };
            let mut failure = state.failure.lock().unwrap_or_else(|e| e.into_inner());
            if failure.is_none() {
                *failure = Some(e);
            }
            ptr::null_mut()
        }
    }
}
pub(crate) unsafe extern "C" fn allocate(opaque: *mut c_void, size: usize) -> *mut c_void {
    allocate_impl(opaque, size, false)
}
pub(crate) unsafe extern "C" fn calloc(opaque: *mut c_void, size: usize) -> *mut c_void {
    allocate_impl(opaque, size, true)
}
pub(crate) unsafe extern "C" fn free(_opaque: *mut c_void, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // Header ownership is transferred back exactly once by the codec.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let base = data.cast::<u8>().sub(std::mem::size_of::<Header>());
        let header = base.cast::<Header>().read();
        let layout = header.layout;
        dealloc(base, layout);
        drop(header);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_charge_headers_zero_memory_and_rollback() {
        let domain = Arc::new(MemoryBudget::new(crate::storage::BudgetLimits {
            total: 8192,
            block: 1024,
            retained: 0,
        }));
        let memory = CodecMemory::new(domain.clone(), ResourceCategory::CodecEncoder);
        unsafe {
            let p = calloc(CodecMemory::opaque(&memory), 4096);
            assert!(!p.is_null()); // Codec workspace is not subject to the payload block limit.
            assert!(std::slice::from_raw_parts(p.cast::<u8>(), 4096)
                .iter()
                .all(|b| *b == 0));
            assert_eq!(
                domain.statistics().current,
                4096 + std::mem::size_of::<Header>() as u64
            );
            assert_eq!(
                domain.detailed_statistics().resources[3].live,
                domain.statistics().current
            );
            let refused = allocate(CodecMemory::opaque(&memory), 8192);
            assert!(refused.is_null());
            assert_ne!(memory.error().kind(), std::io::ErrorKind::WouldBlock);
            assert_eq!(
                domain.statistics().current,
                4096 + std::mem::size_of::<Header>() as u64
            );
            free(CodecMemory::opaque(&memory), p);
            free(CodecMemory::opaque(&memory), ptr::null_mut());
        }
        assert_eq!(domain.statistics().current, 0);
    }
    #[test]
    fn oversized_callback_never_unwinds() {
        let memory = CodecMemory::new(
            Arc::new(MemoryBudget::default()),
            ResourceCategory::CodecDecoder,
        );
        assert!(unsafe { allocate(CodecMemory::opaque(&memory), usize::MAX) }.is_null());
        assert!(memory.take_error().is_some());
    }
}
