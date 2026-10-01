#[path = "budget_statistics.rs"]
mod statistics;
use statistics::DetailedStatistics;
use super::*;
use mcap::charged::ChargedBox;
use std::{cell::UnsafeCell, ptr::NonNull, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Mutex, OnceLock}};
static NEXT: AtomicU64 = AtomicU64::new(1);
type Notification = unsafe extern "C" fn(u64);
#[derive(Default)]
struct Links {
    previous: Option<NonNull<Handle>>,
    next: Option<NonNull<Handle>>,
    pending: Option<NonNull<Handle>>,
    queued: bool,
    callback: Option<Notification>,
}
pub struct Handle {
    id: u64,
    domain: mcap::storage::BudgetRef,
    links: UnsafeCell<Links>,
}
// Links are only accessed while holding REGISTRY; identity/domain are immutable.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
#[derive(Default)]
struct Registry {
    head: Option<NonNull<Handle>>,
    changed: Option<NonNull<Handle>>,
    count: usize,
    queued: usize,
}
unsafe impl Send for Registry {}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static HAS_PENDING: AtomicBool = AtomicBool::new(false);
fn registry() -> &'static Mutex<Registry> { REGISTRY.get_or_init(Default::default) }
impl Registry {
    unsafe fn insert(&mut self, pointer: NonNull<Handle>) {
        let links = &mut *pointer.as_ref().links.get();
        links.next = self.head;
        if let Some(head) = self.head { (*head.as_ref().links.get()).previous = Some(pointer); }
        self.head = Some(pointer); self.count += 1;
    }
    unsafe fn unqueue(&mut self, pointer: NonNull<Handle>) {
        if !(*pointer.as_ref().links.get()).queued { return; }
        let mut cursor = &mut self.changed;
        while let Some(item) = *cursor {
            let links = &mut *item.as_ref().links.get();
            if item == pointer {
                *cursor = links.pending.take(); links.queued = false; self.queued -= 1;
                HAS_PENDING.store(self.queued != 0, Ordering::Release); return;
            }
            cursor = &mut links.pending;
        }
    }
    unsafe fn remove(&mut self, pointer: NonNull<Handle>) {
        self.unqueue(pointer);
        let links = &mut *pointer.as_ref().links.get();
        if let Some(previous) = links.previous { (*previous.as_ref().links.get()).next = links.next; }
        else { self.head = links.next; }
        if let Some(next) = links.next { (*next.as_ref().links.get()).previous = links.previous; }
        self.count -= 1;
    }
}
pub(super) fn publish(domain: &mcap::storage::BudgetRef) -> Outcome<*mut Handle> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let value = ChargedBox::new_fixed(Handle {id, domain: domain.clone(), links: UnsafeCell::new(Links::default())},
        domain, mcap::storage::ResourceCategory::Scratch)?;
    let pointer = NonNull::new(value.into_raw_value()).unwrap();
    unsafe { registry().lock().unwrap().insert(pointer); }
    Ok(pointer.as_ptr())
}
pub fn resolve(id: u64) -> Outcome<mcap::storage::BudgetRef> {
    if id == 0 { return Ok(mcap::storage::BudgetRef::new(Default::default())?); }
    let registry = registry().lock().unwrap();
    let mut cursor = registry.head;
    while let Some(pointer) = cursor {
        let value = unsafe { pointer.as_ref() };
        if value.id == id { return Ok(value.domain.clone()); }
        cursor = unsafe { (*value.links.get()).next };
    }
    Err("Memory budget is no longer available".into())
}
pub fn parse(v: &Value) -> Outcome<mcap::storage::BudgetRef> {
    resolve(v["id"].as_u64().unwrap_or(0))
}
#[no_mangle]
pub unsafe extern "C" fn fm_budget_open(
    total: usize,
    block: usize,
    retained: usize,
    handle: *mut *mut Handle,
    id: *mut u64,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if total == 0 || block == 0 || block > total || retained > total {
            return Err("Invalid memory budget limits".into());
        }
        let h = handle.as_mut().ok_or("Null output")?;
        *h = ptr::null_mut();
        let result = id.as_mut().ok_or("Null ID")?;
        *result = 0;
        let control = ChargedBox::<Handle>::allocation_size()
            .checked_add(mcap::storage::BudgetRef::allocation_size()).ok_or("Control size overflow")?;
        if control > total {
            return Err(mcap::storage::StorageLimit {
                resource: "NativeDomain", limit: total, domain_limit: total, requested: control,
                current: 0, phase: "budget-control",
            }.into());
        }
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            total,
            block,
            retained,
        })?;
        *h = publish(&domain)?;
        *result = (**h).id;
        Ok(0)
    })
}
#[repr(C)]
#[derive(Default)]
pub struct Statistics {
    current: u64,
    peak: u64,
    retained: u64,
    allocations: u64,
    copied: u64,
}
#[no_mangle]
pub unsafe extern "C" fn fm_budget_statistics(
    handle: *const Handle,
    stats: *mut Statistics,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let s = handle.as_ref().ok_or("Null budget")?.domain.statistics();
        *stats.as_mut().ok_or("Null statistics")? = Statistics {
            current: s.current,
            peak: s.peak,
            retained: s.retained,
            allocations: s.allocations,
            copied: s.copied,
        };
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_budget_free(handle: *mut Handle) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // Lock order: detach the ledger hook first, then unlink the registry.
            // No registry lock is held while acquiring the ledger or dropping storage.
            (*handle).domain.set_capacity_observer(None);
            registry().lock().unwrap().remove(NonNull::new_unchecked(handle));
            drop(ChargedBox::from_raw_value(handle));
        }));
    }
}

pub fn unavailable(e: &Error) -> bool {
    if e.advanced() { return false; }
    if let Some(mcap::McapError::Storage(failure))=e.downcast_ref::<mcap::McapError>() {
        return !failure.terminal && failure.kind==mcap::storage::StorageFailureKind::BudgetUnavailable;
    }
    if let Some(mcap::McapError::Io(io)) = e.downcast_ref::<mcap::McapError>() {
        return io.kind() == std::io::ErrorKind::WouldBlock;
    }
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
}

pub fn requested_capacity(error: &Error) -> Option<usize> {
    if let Some(mcap::McapError::Storage(failure))=error.downcast_ref::<mcap::McapError>() {
        return Some(failure.details.requested);
    }
    let io=if let Some(mcap::McapError::Io(io))=error.downcast_ref::<mcap::McapError>() {
        io
    } else {error.downcast_ref::<std::io::Error>()?};
    io.get_ref()?.downcast_ref::<mcap::storage::StorageLimit>().map(|limit|limit.requested)
}

/// A record has already been consumed; storage refusal cannot retry that record.
pub fn after_advance(error: Error) -> Error {
    error.after_advance()
}

#[no_mangle]
pub unsafe extern "C" fn fm_budget_detailed_statistics(
    handle: *const Handle,
    stats: *mut DetailedStatistics,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        *stats.as_mut().ok_or("Null statistics")? = handle
            .as_ref()
            .ok_or("Null budget")?
            .domain
            .detailed_statistics().into();
        Ok(0)
    })
}

// Runs only as native bookkeeping under the domain ledger. Context stays valid
// until fm_budget_free clears the observer under that same ledger lock.
fn changed(context: usize) {
    let pointer = NonNull::new(context as *mut Handle).unwrap();
    let mut registry = registry().lock().unwrap();
    let links = unsafe { &mut *pointer.as_ref().links.get() };
    if !links.queued && links.callback.is_some() {
        links.pending = registry.changed; links.queued = true;
        registry.changed = Some(pointer); registry.queued += 1;
        HAS_PENDING.store(true, Ordering::Release);
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_budget_notify(handle: *const Handle, callback: Notification, out: *mut Response) -> i32 {
    guard(out, |_| {
        let h = handle.as_ref().ok_or("Null budget")?;
        {
            let _registry = registry().lock().unwrap();
            (*h.links.get()).callback = Some(callback);
        }
        h.domain.set_capacity_observer(Some((handle as usize, changed)));
        Ok(0)
    })
}
// Only dispatches domains actually queued by capacity changes. Bound each pass
// by its initial queue length; callbacks and destruction run outside all locks.
pub fn dispatch_notifications() {
    if !HAS_PENDING.load(Ordering::Acquire) { return; }
    let Some(mutex) = REGISTRY.get() else { return; };
    let count = mutex.lock().unwrap().queued;
    for _ in 0..count {
        let next = {
            let mut registry = mutex.lock().unwrap();
            registry.changed.map(|pointer| unsafe {
                let value = pointer.as_ref();
                let links = &mut *value.links.get();
                registry.changed = links.pending.take(); links.queued = false; registry.queued -= 1;
                HAS_PENDING.store(registry.queued != 0, Ordering::Release);
                (value.id, links.callback)
            })
        };
        if let Some((id, Some(callback))) = next { unsafe { callback(id); } }
    }
}
#[no_mangle]
pub extern "C" fn fm_budget_dispatch() {
    let _ = catch_unwind(AssertUnwindSafe(dispatch_notifications));
}

#[cfg(test)]
mod wait_notification_tests {
    use std::sync::Arc;
    use super::*;
    static NOTIFIED:AtomicU64=AtomicU64::new(0);
    unsafe extern "C" fn notified(id:u64) { NOTIFIED.store(id,Ordering::SeqCst); }
    struct Registration(*mut Handle);
    impl Drop for Registration { fn drop(&mut self) { unsafe {fm_budget_free(self.0);} } }
    #[test]
    fn publication_refusal_and_unlink_release_control_storage() {
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {total: (1) + mcap::storage::BudgetRef::allocation_size(), block: 1, retained: 0}).unwrap();
        assert!(publish(&domain).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.fail_allocation_at(0);
        assert!(publish(&domain).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        domain.fail_allocation_at(usize::MAX);
        let handle = publish(&domain).unwrap();
        let id = unsafe {(*handle).id};
        assert!(mcap::storage::BudgetRef::ptr_eq(&resolve(id).unwrap(), &domain));
        assert_eq!(domain.workload_statistics().current as usize, ChargedBox::<Handle>::allocation_size());
        unsafe { fm_budget_free(handle); }
        assert!(resolve(id).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
    }

    #[test]
    fn concurrent_capacity_changes_cannot_access_unregistered_handle() {
        unsafe extern "C" fn callback(_: u64) {}
        for _ in 0..64 {
            let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
            let handle = publish(&domain).unwrap();
            let id = unsafe { (*handle).id };
            let mut response = Response::default();
            unsafe { assert_eq!(fm_budget_notify(handle, callback, &mut response), 0); }
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_domain = domain.clone();
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                for _ in 0..256 { drop(worker_domain.reserve(1).unwrap()); }
            });
            barrier.wait();
            unsafe { fm_budget_free(handle); }
            worker.join().unwrap();
            dispatch_notifications();
            assert!(resolve(id).is_err());
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }

    #[test]
    fn queued_notifications_are_coalesced_reentrant_and_removed_on_free() {
        static CALLS: AtomicU64 = AtomicU64::new(0);
        unsafe extern "C" fn callback(id: u64) {
            CALLS.fetch_add(1, Ordering::SeqCst);
            // Reentry into the native domain queues another notification without
            // deadlocking or extending this dispatch pass indefinitely.
            if let Ok(domain) = resolve(id) { drop(domain.reserve(1).unwrap()); }
        }
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let handle = publish(&domain).unwrap();
        let mut response = Response::default();
        unsafe { assert_eq!(fm_budget_notify(handle, callback, &mut response), 0); }
        for _ in 0..100 { drop(domain.reserve(1).unwrap()); }
        let before = CALLS.load(Ordering::SeqCst);
        dispatch_notifications();
        assert!(CALLS.load(Ordering::SeqCst) > before);
        unsafe { fm_budget_free(handle); }
        dispatch_notifications();
        let registry = registry().lock().unwrap();
        let mut cursor = registry.changed;
        while let Some(pointer) = cursor {
            assert_ne!(pointer.as_ptr(), handle);
            cursor = unsafe { (*pointer.as_ref().links.get()).pending };
        }
        drop(registry);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn release_then_reoccupation_notifies_the_latched_domain_ticket() {
        let domain=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {total:(65536) + mcap::storage::BudgetRef::allocation_size(),block:65536,retained:0}).unwrap();
        let handle = publish(&domain).unwrap();
        let id = unsafe {(*handle).id};
        let _registration = Registration(handle);
        let baseline = domain.workload_statistics().current;
        let mut ticket=mcap::storage::CapacityWaitTicket::new(&domain).unwrap();
        let count=65536-domain.workload_statistics().current as usize;
        let occupied=domain.reserve(count).unwrap();
        ticket.arm(4096).unwrap();
        let mut response=Response::default();
        unsafe {assert_eq!(fm_budget_notify(handle,notified,&mut response),0);}
        let unrelated=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        drop(unrelated.reserve(4096).unwrap());
        dispatch_notifications();
        assert_ne!(NOTIFIED.load(Ordering::SeqCst),id);
        drop(occupied);
        let replacement=domain.reserve(count).unwrap();
        assert_eq!(domain.workload_statistics().current,65536);
        dispatch_notifications();
        assert_eq!(NOTIFIED.load(Ordering::SeqCst),id);
        assert_eq!(ticket.status(),mcap::storage::CapacityWaitStatus::Ready);
        ticket.cancel();
        drop((replacement,ticket));
        assert_eq!(domain.workload_statistics().current,baseline);
    }
}

#[cfg(test)]
mod fixed_failure_tests {
    use super::*;
    #[test]
    fn retry_classification_preserves_fixed_failure_terminal_state() {
        use mcap::storage::{StorageFailure,StorageFailureKind,StorageLimit};
        for kind in [StorageFailureKind::BudgetUnavailable,StorageFailureKind::PermanentLimit,StorageFailureKind::SystemAllocation] {
            for terminal in [false,true] {
                let failure=StorageFailure {kind,terminal,details:StorageLimit {resource:"NativeDomain",limit:1000,domain_limit:1000,requested:128,current:900,phase:"reservation"}};
                let error:Error=failure.into();
                assert_eq!(unavailable(&error),kind==StorageFailureKind::BudgetUnavailable && !terminal);
                assert_eq!(requested_capacity(&error),Some(128));
                let error=after_advance(error);
                assert!(!unavailable(&error));
                assert_eq!(requested_capacity(&error),Some(128));
            }
        }
    }
}
