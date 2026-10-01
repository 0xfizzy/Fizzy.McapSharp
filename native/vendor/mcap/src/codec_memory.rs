//! Private allocator callbacks for the pinned codec C APIs.
use crate::storage::{Reservation, ResourceCategory, StorageFailure, StorageFailureKind};
use std::{
    alloc::{alloc, dealloc, Layout},
    ffi::c_void,
    ptr,
    sync::Mutex,
};

pub(crate) struct CodecMemory {
    budget: crate::storage::BudgetRef,
    category: ResourceCategory,
    failure: Mutex<Option<StorageFailure>>,
}
#[repr(C, align(16))]
struct Header {
    charge: Reservation,
    layout: Layout,
}
impl CodecMemory {
    #[cfg(any(test, feature = "allocation-audit"))]
    pub fn new(
        budget: crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> std::io::Result<crate::charged::ChargedBox<Self>> {
        Self::new_fixed(budget, category).map_err(StorageFailure::into_io)
    }
    pub fn new_fixed(budget: crate::storage::BudgetRef, category: ResourceCategory)
        -> Result<crate::charged::ChargedBox<Self>, StorageFailure> {
        crate::charged::ChargedBox::new_fixed(
            Self {budget: budget.clone(), category, failure: Mutex::new(None)}, &budget, category)
            .map_err(StorageFailure::terminal)
    }
    pub fn opaque(this: &crate::charged::ChargedBox<Self>) -> *mut c_void {
        (&**this as *const Self).cast_mut().cast()
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
    fn failure(&self, requested: usize, phase: &'static str, kind: StorageFailureKind) -> StorageFailure {
        StorageFailure::at(&self.budget, self.category, requested, phase, kind).terminal()
    }
    #[cfg(test)]
    pub fn error(&self) -> std::io::Error { self.fixed_error().into_io() }
    #[cfg(test)]
    pub fn take_error(&self) -> Option<std::io::Error> { self.take_failure().map(StorageFailure::into_io) }
    pub fn fixed_error(&self) -> StorageFailure {
        self.take_failure().unwrap_or_else(|| self.failure(0, "codec context", StorageFailureKind::SystemAllocation))
    }
    pub fn take_failure(&self) -> Option<StorageFailure> {
        self.failure.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
    fn allocate(&self, size: usize, zero: bool) -> Result<*mut c_void, StorageFailure> {
        let total = size.max(1).checked_add(std::mem::size_of::<Header>())
            .ok_or_else(|| self.failure(size, "codec layout", StorageFailureKind::Overflow))?;
        let layout = Layout::from_size_align(total, std::mem::align_of::<Header>())
            .map_err(|_| self.failure(size, "codec layout", StorageFailureKind::Overflow))?;
        let mut charge = self.budget.reserve_class_fixed(total, self.category)
            .map_err(|error| StorageFailure::from(error).terminal())?;
        self.budget.allocation_attempt()
            .map_err(|_| self.failure(total, "codec allocation", StorageFailureKind::SystemAllocation))?;
        let p = unsafe { alloc(layout) };
        if p.is_null() {
            return Err(self.failure(total, "codec allocation", StorageFailureKind::SystemAllocation));
        }
        charge.commit(total);
        self.budget.allocation_committed(p, total, self.category);
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
                _ => state.failure(size, "codec callback", StorageFailureKind::CodecPanic),
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

/// Explicit test instrumentation, absent from production. It drives the real C
/// callbacks without publishing pointers or converting failures into I/O boxes.
#[cfg(feature = "allocation-audit")]
pub mod allocation_probe {
    use super::*;
    #[cfg(all(feature = "lz4", feature = "zstd"))]
    pub fn encode(domain: crate::storage::BudgetRef, kind: u32, payload: &[u8]) -> crate::McapResult<()> {
        if kind == 0 {
            let mut encoder = crate::codec_writer::lz4_encoder::Encoder::new(std::io::sink(), 0, domain)?;
            encoder.write_all(payload)?;
            encoder.flush()?;
            encoder.finish().1
        } else {
            let mut encoder = crate::codec_writer::zstd_encoder::Encoder::new(std::io::sink(), 0, kind - 1, domain)?;
            encoder.write_all(payload)?;
            encoder.flush()?;
            encoder.finish().1
        }
    }
    #[derive(Debug)]
    pub enum Failure {
        Capacity(crate::storage::StorageLimit),
        Overflow,
        Allocation(std::io::ErrorKind),
        Panic,
    }
    pub struct Probe(crate::charged::ChargedBox<CodecMemory>);
    impl Probe {
        pub fn new(domain: crate::storage::BudgetRef, category: ResourceCategory) -> std::io::Result<Self> {
            Ok(Self(CodecMemory::new(domain, category)?))
        }
        pub fn exercise(&self, bytes: usize, zero: bool) -> Result<usize, Failure> {
            let state = CodecMemory::opaque(&self.0);
            let pointer = unsafe { allocate_impl(state, bytes, zero) };
            if pointer.is_null() {
                let failure = self.0.take_failure().expect("callback records its failure");
                return Err(match failure.kind {
                    StorageFailureKind::PermanentLimit | StorageFailureKind::BudgetUnavailable => Failure::Capacity(failure.details),
                    StorageFailureKind::Overflow => Failure::Overflow,
                    StorageFailureKind::SystemAllocation => Failure::Allocation(std::io::ErrorKind::OutOfMemory),
                    StorageFailureKind::CodecPanic => Failure::Panic,
                });
            }
            let capacity = unsafe {
                let header = &*pointer.cast::<u8>().sub(std::mem::size_of::<Header>()).cast::<Header>();
                if zero { assert!(std::slice::from_raw_parts(pointer.cast::<u8>(), bytes).iter().all(|b| *b == 0)); }
                header.layout.size()
            };
            unsafe { free(state, pointer) };
            Ok(capacity)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_failure_preserves_first_callback_and_terminal_capacity_details() {
        let domain = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: 4096 + crate::storage::BudgetRef::allocation_size(), block: 4096, retained: 0,
        }).unwrap();
        let memory = CodecMemory::new(domain.clone(), ResourceCategory::CodecDecoder).unwrap();
        let occupied = domain.reserve(3000).unwrap();
        let before = domain.statistics().current;
        unsafe {
            // The first failed request is retained even if the codec makes
            // another allocation before returning control to Rust.
            assert!(allocate(CodecMemory::opaque(&memory), 2048).is_null());
            assert!(allocate(CodecMemory::opaque(&memory), usize::MAX).is_null());
        }
        assert_eq!(domain.statistics().current, before);
        let error = memory.error();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        let limit = error.get_ref().unwrap().downcast_ref::<crate::storage::StorageLimit>().unwrap();
        assert_eq!(limit.resource, "CodecDecoder");
        assert_eq!(limit.requested, 2048 + std::mem::size_of::<Header>());
        assert_eq!(limit.limit, domain.limits().total);
        assert_eq!(limit.current, before as usize);
        assert_eq!(limit.phase, "reservation");
        assert!(memory.take_failure().is_none());
        drop(occupied);
        drop(memory);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn callbacks_charge_headers_zero_memory_and_rollback() {
        let domain = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: (8192) + crate::storage::BudgetRef::allocation_size(),
            block: 1024,
            retained: 0,
        }).unwrap();
        let memory = CodecMemory::new(domain.clone(), ResourceCategory::CodecEncoder).unwrap();
        let state_bytes = domain.workload_statistics().current;
        unsafe {
            let p = calloc(CodecMemory::opaque(&memory), 4096);
            assert!(!p.is_null()); // Codec workspace is not subject to the payload block limit.
            assert!(std::slice::from_raw_parts(p.cast::<u8>(), 4096)
                .iter()
                .all(|b| *b == 0));
            assert_eq!(
                domain.workload_statistics().current,
                state_bytes + 4096 + std::mem::size_of::<Header>() as u64
            );
            assert_eq!(
                domain.workload_detailed_statistics().resources[3].live,
                domain.workload_statistics().current
            );
            let refused = allocate(CodecMemory::opaque(&memory), 8192);
            assert!(refused.is_null());
            assert_ne!(memory.error().kind(), std::io::ErrorKind::WouldBlock);
            assert_eq!(
                domain.workload_statistics().current,
                state_bytes + 4096 + std::mem::size_of::<Header>() as u64
            );
            free(CodecMemory::opaque(&memory), p);
            free(CodecMemory::opaque(&memory), ptr::null_mut());
        }
        assert_eq!(domain.workload_statistics().current, state_bytes);
        drop(memory);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn oversized_callback_never_unwinds() {
        let memory = CodecMemory::new(
            crate::storage::BudgetRef::new(Default::default()).unwrap(),
            ResourceCategory::CodecDecoder,
        )
        .unwrap();
        assert!(unsafe { allocate(CodecMemory::opaque(&memory), usize::MAX) }.is_null());
        assert!(memory.take_error().is_some());
    }
}

#[cfg(all(test, feature = "zstd", feature = "lz4"))]
mod failure_matrix {
    use super::*;
    fn encode(domain: crate::storage::BudgetRef, kind: u32) -> crate::McapResult<()> {
        let payload = [37u8; 65536];
        if kind == 0 {
            let mut encoder =
                crate::codec_writer::lz4_encoder::Encoder::new(std::io::sink(), 0, domain)?;
            encoder.write_all(&payload)?;
            encoder.flush()?;
            encoder.finish().1
        } else {
            let mut encoder = crate::codec_writer::zstd_encoder::Encoder::new(
                std::io::sink(),
                0,
                kind - 1,
                domain,
            )?;
            encoder.write_all(&payload)?;
            encoder.flush()?;
            encoder.finish().1
        }
    }
    #[test]
    fn every_observed_encoder_allocation_failure_releases_domain() {
        for kind in 0..4 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            encode(domain.clone(), kind).unwrap();
            let attempts = domain.allocation_attempt_count();
            assert!(attempts > 2);
            assert_eq!(domain.workload_statistics().current, 0);
            for index in 0..attempts {
                eprintln!("codec failure injection kind={kind} index={index}/{attempts}");
                domain.fail_allocation_at(index);
                let result = encode(domain.clone(), kind);
                if domain.allocation_attempt_count() > index {
                    assert!(result.is_err(), "kind={kind} index={index}");
                }
                assert_eq!(domain.workload_statistics().current, 0, "kind={kind} index={index}");
                assert_eq!(
                    domain.workload_detailed_statistics().resources[ResourceCategory::CodecEncoder as usize]
                        .live,
                    0
                );
            }
        }
    }
}
