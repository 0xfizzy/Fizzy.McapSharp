//! Explicit domain root layout. The ledger survives the last strong owner so
//! detached weak controls can finish accounting before the root is deallocated.
use super::{BudgetLimits, MemoryBudget, ResourceCategory, StorageLimit};
use std::{
    alloc::{alloc, dealloc, Layout},
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

struct Root {
    strong: AtomicUsize,
    // One implicit weak reference protects last-strong cleanup.
    weak: AtomicUsize,
    domain: MemoryBudget,
}

/// A bootstrap failure is a fixed-size value, including when no heap budget is
/// available. Conversion into other error representations belongs to the caller.
#[derive(Debug)]
pub enum BootstrapError {
    Limit(StorageLimit),
    Allocation(std::io::ErrorKind),
}
impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit(limit) => limit.fmt(f),
            Self::Allocation(kind) => write!(f, "Domain allocation failed: {kind:?}"),
        }
    }
}
impl std::error::Error for BootstrapError {}
impl From<BootstrapError> for std::io::Error {
    fn from(error: BootstrapError) -> Self {
        match error {
            BootstrapError::Limit(limit) => std::io::Error::other(limit),
            BootstrapError::Allocation(kind) => kind.into(),
        }
    }
}

/// Strong domain ownership, independent of the standard library's Arc layout.
pub struct BudgetRef(NonNull<Root>);
/// A weak control keeps the accounting ledger alive but cannot resurrect a
/// domain after its last strong owner has gone away.
pub struct BudgetWeak(NonNull<Root>);
// Root fields are immutable or synchronized. Raw pointers only identify a root
// retained by this object's strong/weak count; no unsynchronized mutable access.
unsafe impl Send for BudgetRef {}
unsafe impl Sync for BudgetRef {}
unsafe impl Send for BudgetWeak {}
unsafe impl Sync for BudgetWeak {}

fn increment(counter: &AtomicUsize) {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < isize::MAX as usize).then_some(n + 1)
        })
        .expect("Domain reference count overflow");
}

impl BudgetRef {
    pub fn new(limits: BudgetLimits) -> Result<Self, BootstrapError> {
        Self::from_unpublished(MemoryBudget::new(limits))
    }
    pub fn try_default() -> std::io::Result<Self> {
        Self::new(BudgetLimits::default()).map_err(Into::into)
    }
    pub fn allocation_size() -> usize {
        Layout::new::<Root>().size()
    }
    pub fn as_ptr(&self) -> *const MemoryBudget {
        &**self
    }
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
    pub fn downgrade(&self) -> BudgetWeak {
        increment(&unsafe { self.0.as_ref() }.weak);
        BudgetWeak(self.0)
    }
    /// Allocation-test baseline: the root is physically live for the duration
    /// of a workload. Root-inclusive public statistics remain unchanged.
    #[cfg(any(test, feature = "allocation-audit"))]
    pub fn workload_statistics(&self) -> super::BudgetStatistics {
        let mut result = self.statistics();
        let root = Self::allocation_size() as u64;
        assert!(result.current >= root);
        result.current -= root;
        result.peak -= root;
        result.allocations -= 1;
        result
    }
    #[cfg(any(test, feature = "allocation-audit"))]
    pub fn workload_detailed_statistics(&self) -> super::DetailedStatistics {
        let mut result = self.detailed_statistics();
        let root = Self::allocation_size() as u64;
        let control = &mut result.resources[ResourceCategory::Scratch as usize];
        assert!(control.live >= root);
        control.live -= root;
        control.current -= root;
        control.peak -= root;
        result.current_bytes -= root;
        result.peak_bytes -= root;
        result.allocation_count -= 1;
        result.allocated_bytes -= root;
        result
    }

    fn from_unpublished(mut domain: MemoryBudget) -> Result<Self, BootstrapError> {
        let layout = Layout::new::<Root>();
        if layout.size() > domain.limits.total {
            return Err(BootstrapError::Limit(StorageLimit {
                resource: "NativeDomain",
                limit: domain.limits.total,
                domain_limit: domain.limits.total,
                requested: layout.size(),
                current: 0,
                phase: "domain bootstrap",
            }));
        }
        // The root is still on the stack. Reserve before the allocator can run;
        // it has no self-owning Reservation and cannot create a reference cycle.
        let state = domain.state.get_mut().unwrap();
        state.stats.current = layout.size() as u64;
        state.stats.peak = state.stats.current;
        state.stats.allocations = 1;
        let category = &mut state.detailed.resources[ResourceCategory::Scratch as usize];
        category.current = layout.size() as u64;
        category.peak = category.current;
        category.reserved = category.current;
        domain
            .allocation_attempt()
            .map_err(|e| BootstrapError::Allocation(e.kind()))?;
        let pointer = NonNull::new(unsafe { alloc(layout) }.cast::<Root>())
            .ok_or(BootstrapError::Allocation(std::io::ErrorKind::OutOfMemory))?;
        // No fallible operations between allocation and publication. The exact
        // Layout is also the deallocation layout; no allocator rounding guesses.
        let state = domain.state.get_mut().unwrap();
        let category = &mut state.detailed.resources[ResourceCategory::Scratch as usize];
        category.live = category.current;
        category.reserved = 0;
        state.detailed.allocation_count = 1;
        state.detailed.allocated_bytes = layout.size() as u64;
        unsafe {
            pointer.as_ptr().write(Root {
                strong: AtomicUsize::new(1),
                weak: AtomicUsize::new(1),
                domain,
            });
        }
        let result = Self(pointer);
        result.allocation_committed(
            pointer.as_ptr().cast(),
            layout.size(),
            ResourceCategory::Scratch,
        );
        Ok(result)
    }

    /// Diagnostic instrumentation, installed before bootstrap so the independent
    /// allocator can reconcile the root allocation itself.
    #[cfg(feature = "allocation-audit")]
    pub fn new_observed(
        limits: BudgetLimits,
        observer: fn(usize, usize, usize, usize, usize),
    ) -> Result<Self, BootstrapError> {
        let domain = MemoryBudget::new(limits);
        domain.observe_allocations(observer);
        Self::from_unpublished(domain)
    }
}
impl Deref for BudgetRef {
    type Target = MemoryBudget;
    fn deref(&self) -> &MemoryBudget {
        &unsafe { self.0.as_ref() }.domain
    }
}
// Compatibility for upstream infallible constructors. Native binding entry
// points construct their domain with new() and propagate bootstrap failure.
impl Default for BudgetRef {
    fn default() -> Self {
        Self::new(BudgetLimits::default()).expect("Domain bootstrap failed")
    }
}
impl Clone for BudgetRef {
    fn clone(&self) -> Self {
        increment(&unsafe { self.0.as_ref() }.strong);
        Self(self.0)
    }
}
impl std::fmt::Debug for BudgetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}
impl BudgetWeak {
    pub fn upgrade(&self) -> Option<BudgetRef> {
        unsafe { self.0.as_ref() }
            .strong
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |n| {
                if n == 0 {
                    None
                } else {
                    assert!(n < isize::MAX as usize, "Domain reference count overflow");
                    Some(n + 1)
                }
            })
            .ok()
            .map(|_| BudgetRef(self.0))
    }
    // Accounting-only access. The weak count retains the entire ledger even
    // when upgrade fails. Never use this to create new allocations or owners.
    pub(super) fn ledger(&self) -> &MemoryBudget {
        &unsafe { self.0.as_ref() }.domain
    }
}
impl Clone for BudgetWeak {
    fn clone(&self) -> Self {
        increment(&unsafe { self.0.as_ref() }.weak);
        Self(self.0)
    }
}
impl Drop for BudgetWeak {
    fn drop(&mut self) {
        if unsafe { self.0.as_ref() }
            .weak
            .fetch_sub(1, Ordering::Release)
            != 1
        {
            return;
        }
        fence(Ordering::Acquire);
        // Last weak: last-strong cleanup already broke all internal weak cycles.
        // No observer can see a decremented root charge before physical free.
        unsafe {
            std::ptr::drop_in_place(self.0.as_ptr());
            dealloc(self.0.as_ptr().cast(), Layout::new::<Root>());
        }
    }
}
impl Drop for BudgetRef {
    fn drop(&mut self) {
        if unsafe { self.0.as_ref() }
            .strong
            .fetch_sub(1, Ordering::Release)
            != 1
        {
            return;
        }
        fence(Ordering::Acquire);
        // RAII releases the implicit weak even if cleanup unwinds. The ledger
        // itself is not dropped yet: concurrent detached controls still need it.
        let implicit = BudgetWeak(self.0);
        self.set_capacity_observer(None);
        let registry = std::mem::take(&mut *self.reclaimers.lock().unwrap());
        drop(registry);
        self.release_idle_for(self.limits.total);
        drop(implicit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bootstrap_and_no_weak_resurrection() {
        let size = BudgetRef::allocation_size();
        for total in [0, size - 1] {
            let error = BudgetRef::new(BudgetLimits {
                total,
                block: 1,
                retained: 0,
            })
            .unwrap_err();
            let BootstrapError::Limit(error) = error else {
                panic!("wrong failure kind")
            };
            assert_eq!(
                (error.limit, error.requested, error.current),
                (total, size, 0)
            );
            assert_eq!(error.phase, "domain bootstrap");
        }
        // Root controls do not use the payload-block ceiling.
        let root = BudgetRef::new(BudgetLimits {
            total: size,
            block: 1,
            retained: 0,
        })
        .unwrap();
        let stats = root.detailed_statistics();
        assert_eq!(stats.current_bytes, size as u64);
        assert_eq!(stats.allocation_count, 1);
        assert_eq!(stats.allocated_bytes, size as u64);
        assert_eq!(
            stats.resources.iter().map(|c| c.current).sum::<u64>(),
            size as u64
        );
        assert_eq!(stats.resources[8].live, size as u64);
        assert_eq!(stats.resources[8].reserved, 0);
        let weak = root.downgrade();
        let clone = root.clone();
        assert!(clone.ptr_eq(&weak.upgrade().unwrap()));
        drop(root);
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(weak.upgrade().is_none());
        // Still physically allocated and therefore still charged.
        assert_eq!(
            weak.ledger().detailed_statistics().resources[8].live,
            size as u64
        );
        drop(weak);
    }
    #[test]
    fn bootstrap_allocation_failure_is_fallible() {
        let domain = MemoryBudget::default();
        domain.fail_allocation_at(0);
        assert!(matches!(
            BudgetRef::from_unpublished(domain),
            Err(BootstrapError::Allocation(std::io::ErrorKind::OutOfMemory))
        ));
    }
    #[test]
    fn maximum_domain_limit_cannot_wrap_reservations_or_skip_idle_cleanup() {
        let root = BudgetRef::new(BudgetLimits {
            total: usize::MAX,
            block: 65536,
            retained: 65536,
        })
        .unwrap();
        let error = root
            .reserve(usize::MAX)
            .err()
            .expect("root capacity must remain charged");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            root.statistics().current,
            BudgetRef::allocation_size() as u64
        );
        let mut input =
            super::super::WriteBuffer::with_budget(root.clone(), ResourceCategory::Input);
        input.reserve(4096, 0..0).unwrap();
        drop(input);
        assert!(root.statistics().retained > 4096);
        let weak = root.downgrade();
        drop(root);
        assert!(weak.upgrade().is_none());
        assert_eq!(weak.ledger().statistics().retained, 0);
        assert_eq!(
            weak.ledger().statistics().current,
            BudgetRef::allocation_size() as u64
        );
    }
    #[test]
    fn concurrent_weak_upgrade_and_last_strong_release() {
        for _ in 0..128 {
            let root = BudgetRef::new(BudgetLimits::default()).unwrap();
            let weak = root.downgrade();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let worker = weak.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..256 {
                        if let Some(owner) = worker.upgrade() {
                            assert_eq!(
                                owner.statistics().current,
                                BudgetRef::allocation_size() as u64
                            );
                        }
                    }
                });
                barrier.wait();
                drop(root);
            });
            assert!(weak.upgrade().is_none());
            assert_eq!(
                weak.ledger().statistics().current,
                BudgetRef::allocation_size() as u64
            );
        }
    }
}
