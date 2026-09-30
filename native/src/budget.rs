use super::*;
pub use mcap::storage::MemoryBudget;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, OnceLock, Weak,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
static DOMAINS: OnceLock<Mutex<BTreeMap<u64, Weak<MemoryBudget>>>> = OnceLock::new();
pub struct Handle {
    id: u64,
    domain: Arc<MemoryBudget>,
}
fn registry() -> &'static Mutex<BTreeMap<u64, Weak<MemoryBudget>>> {
    DOMAINS.get_or_init(Default::default)
}
pub fn parse(v: &Value) -> Outcome<Arc<MemoryBudget>> {
    if let Some(id) = v["id"].as_u64() {
        return registry()
            .lock()
            .unwrap()
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| "Memory budget is no longer available".into());
    }
    Ok(Arc::new(MemoryBudget::default()))
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
        let domain = Arc::new(MemoryBudget::new(mcap::storage::BudgetLimits {
            total,
            block,
            retained,
        }));
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        registry()
            .lock()
            .unwrap()
            .insert(id, Arc::downgrade(&domain));
        *h = Box::into_raw(Box::new(Handle { id, domain }));
        *result = id;
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
            let h = Box::from_raw(handle);
            registry().lock().unwrap().remove(&h.id);
            if let Some(listeners) = LISTENERS.get() {
                listeners.lock().unwrap().remove(&h.id);
            }
            drop(h);
        }));
    }
}

pub fn unavailable(e: &Error) -> bool {
    if let Some(mcap::McapError::Io(io)) = e.downcast_ref::<mcap::McapError>() {
        return io.kind() == std::io::ErrorKind::WouldBlock;
    }
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
}

#[no_mangle]
pub unsafe extern "C" fn fm_budget_detailed_statistics(
    handle: *const Handle,
    stats: *mut mcap::storage::DetailedStatistics,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        *stats.as_mut().ok_or("Null statistics")? = handle
            .as_ref()
            .ok_or("Null budget")?
            .domain
            .detailed_statistics();
        Ok(0)
    })
}

type Notification = unsafe extern "C" fn(u64);
struct Listener {
    domain: Weak<MemoryBudget>,
    callback: Notification,
    capacity_version: u64,
    current: u64,
    retained: u64,
    leases: usize,
}
static LISTENERS: OnceLock<Mutex<BTreeMap<u64, Listener>>> = OnceLock::new();
#[no_mangle]
pub unsafe extern "C" fn fm_budget_notify(
    handle: *const Handle,
    callback: Notification,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = handle.as_ref().ok_or("Null budget")?;
        LISTENERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(
                h.id,
                Listener {
                    domain: Arc::downgrade(&h.domain),
                    callback,
                    capacity_version: h.domain.capacity_version(),
                    current: h.domain.statistics().current,
                    retained: h.domain.statistics().retained,
                    leases: h.domain.lease_count(),
                },
            );
        Ok(0)
    })
}
// Called only after native operations return from codec/parser code, never from allocator callbacks.
pub fn dispatch_notifications() {
    let Some(listeners) = LISTENERS.get() else {
        return;
    };
    let mut after = 0;
    loop {
        let next = {
            let mut listeners = listeners.lock().unwrap();
            listeners
                .range_mut((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                .next()
                .map(|(&id, l)| {
                    let notify = if let Some(domain) = l.domain.upgrade() {
                        let version = domain.capacity_version();
                        let stats = domain.statistics();
                        let leases = domain.lease_count();
                        // Failed operations can release their own descriptor reservations.
                        // A version alone must not make a blocked retry wake itself forever.
                        let changed = version != l.capacity_version
                            && (stats.current < l.current
                                || stats.retained > l.retained
                                || leases < l.leases);
                        l.capacity_version = version;
                        l.current = stats.current;
                        l.retained = stats.retained;
                        l.leases = leases;
                        changed
                    } else {
                        false
                    };
                    (id, l.callback, notify)
                })
        };
        let Some((id, callback, notify)) = next else {
            break;
        };
        after = id;
        if notify {
            unsafe {
                callback(id);
            }
        }
    }
}
#[no_mangle]
pub extern "C" fn fm_budget_dispatch() {
    let _ = catch_unwind(AssertUnwindSafe(dispatch_notifications));
}

const _: [(); 456] = [(); std::mem::size_of::<mcap::storage::DetailedStatistics>()];
