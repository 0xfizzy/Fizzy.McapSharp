//! Stable byte storage for parsers. Shared ranges are immutable; producers only append
//! to unpublished bytes. An allocation is never compacted or reused while shared.
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

#[path = "storage_pool.rs"]
mod pool;
#[path = "capacity_wait.rs"]
mod capacity_wait;
#[path = "budget_owner.rs"]
mod budget_owner;
pub use budget_owner::{BudgetRef, BudgetWeak, BootstrapError};
pub use capacity_wait::{CapacityWaitTicket, CapacityWaitStatus, CapacityReleaseGuard};
use crate::charged::weak::WeakReclaimer;
use pool::Allocation;

#[derive(Clone, Copy, Debug)]
pub struct StorageLimit {
    pub resource: &'static str,
    pub limit: usize,
    pub domain_limit: usize,
    pub requested: usize,
    pub current: usize,
    pub phase: &'static str,
}
impl std::fmt::Display for StorageLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} memory limit {} exceeded by requested capacity {}",
            self.resource, self.limit, self.requested
        )
    }
}
impl std::error::Error for StorageLimit {}

/// Fixed-size allocation failure, usable even when the budget is exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageFailureKind { PermanentLimit, BudgetUnavailable, SystemAllocation, Overflow, CodecPanic }
impl StorageFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermanentLimit => "permanent", Self::BudgetUnavailable => "temporary",
            Self::SystemAllocation => "system", Self::Overflow => "overflow", Self::CodecPanic => "codec-panic",
        }
    }
}
impl std::fmt::Display for StorageFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
}
#[derive(Clone, Copy, Debug)]
pub struct StorageFailure {
    pub details: StorageLimit,
    pub kind: StorageFailureKind,
    /// A codec may have advanced even when its allocation request was refused.
    pub terminal: bool,
}
impl StorageFailure {
    pub(crate) fn at(domain: &BudgetRef, category: ResourceCategory, requested: usize,
        phase: &'static str, kind: StorageFailureKind) -> Self {
        Self { details: StorageLimit {resource: match category {
            ResourceCategory::CodecEncoder => "CodecEncoder", ResourceCategory::CodecDecoder => "CodecDecoder",
            _ => "NativeDomain",
        }, limit: domain.limits().total, domain_limit: domain.limits().total, requested, current: domain.statistics().current as usize, phase}, kind, terminal: false }
    }
    pub(crate) fn terminal(mut self) -> Self { self.terminal = true; self }
    // Compatibility only; fixed-result callers do not use this allocating bridge.
    pub(crate) fn into_io(self) -> std::io::Error {
        match self.kind {
            StorageFailureKind::SystemAllocation => std::io::ErrorKind::OutOfMemory.into(),
            StorageFailureKind::Overflow => std::io::Error::other("Codec allocation size overflow"),
            StorageFailureKind::CodecPanic => std::io::Error::other("Codec allocator panic"),
            _ => std::io::Error::new(
                if self.kind == StorageFailureKind::BudgetUnavailable && !self.terminal {
                    std::io::ErrorKind::WouldBlock
                } else { std::io::ErrorKind::Other }, self.details),
        }
    }
}
impl std::fmt::Display for StorageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            StorageFailureKind::PermanentLimit | StorageFailureKind::BudgetUnavailable => self.details.fmt(f),
            _ => write!(f, "{} {} failure requesting {} bytes during {}", self.details.resource,
                self.kind.as_str(), self.details.requested, self.details.phase),
        }
    }
}
impl std::error::Error for StorageFailure {}

/// An allocation-free failure from the reservation transaction. Conversion to
/// the compatibility I/O error is deliberately outside the ledger lock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReservationFailure {
    pub kind: std::io::ErrorKind,
    pub limit: StorageLimit,
}
impl From<ReservationFailure> for StorageFailure {
    fn from(failure: ReservationFailure) -> Self {
        Self { details: failure.limit, kind: if failure.kind == std::io::ErrorKind::WouldBlock {
            StorageFailureKind::BudgetUnavailable
        } else { StorageFailureKind::PermanentLimit }, terminal: false }
    }
}
impl ReservationFailure {
    pub fn into_io(self) -> std::io::Error {
        std::io::Error::new(self.kind, self.limit)
    }
}


#[derive(Clone, Copy, Debug)]
pub struct BudgetLimits {
    pub total: usize,
    pub block: usize,
    pub retained: usize,
}
impl Default for BudgetLimits {
    fn default() -> Self {
        Self {
            total: 256 << 20,
            block: 64 << 20,
            retained: 64 << 20,
        }
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct BudgetStatistics {
    pub current: u64,
    pub peak: u64,
    pub retained: u64,
    pub allocations: u64,
    pub copied: u64,
}
#[repr(usize)]
#[derive(Clone, Copy, Debug)]
pub enum ResourceCategory {
    Input,
    Decompressed,
    Writer,
    CodecEncoder,
    CodecDecoder,
    Index,
    Descriptor,
    Declaration,
    Scratch,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct ResourceStatistics {
    pub current: u64,
    pub peak: u64,
    pub live: u64,
    pub reserved: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct DetailedStatistics {
    pub resources: [ResourceStatistics; 9],
    pub allocation_count: u64,
    pub allocated_bytes: u64,
    pub reallocation_count: u64,
    pub rejected: u64,
    pub flow: FlowStatistics,
    pub lease_payload_bytes: u64,
    pub cache_payload_bytes: u64,
    pub current_bytes: u64,
    pub peak_bytes: u64,
    pub idle_bytes: u64,
    pub immediately_reclaimable_bytes: u64,
    pub mapped_logical_bytes: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct FlowStatistics {
    pub input_copy: u64,
    pub compaction_copy: u64,
    pub delivery_copy: u64,
    pub other_copy: u64,
    pub encoded_input: u64,
    pub encoded_output: u64,
    pub decoded_input: u64,
    pub decoded_output: u64,
    pub decode_started: u64,
    pub decode_completed: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_evictions: u64,
    pub reclaimed_bytes: u64,
}
#[derive(Clone, Copy)]
pub enum CopyKind {
    Input,
    Compaction,
    Delivery,
    Other,
}
#[derive(Default)]
struct State {
    stats: BudgetStatistics,
    detailed: DetailedStatistics,
    ownership: OwnershipStatistics,
    mapped_logical_bytes: u64,
    free: pool::IdlePool,
    waiters: capacity_wait::Waiters,
    active_releases: usize,
    capacity_observer: Option<(usize, fn(usize))>,
}
impl State {
    fn available_capacity(&self, total: usize) -> usize {
        total.saturating_sub(self.stats.current as usize)
            .saturating_add(self.stats.retained as usize)
            .saturating_add(self.ownership.immediately_reclaimable as usize)
    }
    fn add_live(&mut self, category: ResourceCategory, bytes: usize, new: bool) {
        let c = &mut self.detailed.resources[category as usize];
        c.current += bytes as u64;
        c.live += bytes as u64;
        c.peak = c.peak.max(c.current);
        if new {
            self.detailed.allocation_count += 1;
            self.detailed.allocated_bytes += bytes as u64;
        }
    }
    fn remove_live(&mut self, category: ResourceCategory, bytes: usize) {
        let c = &mut self.detailed.resources[category as usize];
        c.current -= bytes as u64;
        c.live -= bytes as u64;
    }
}
pub trait Reclaimable: Send + Sync {
    fn last_access(&self) -> u64;
    fn reclaim(&self) -> bool;
}
const REGISTRY_PAGE_ENTRIES: usize = 64;
struct RegistryPage {
    entries: [Option<WeakReclaimer>; REGISTRY_PAGE_ENTRIES],
    next: Option<RegistryAllocation>,
    _charge: std::mem::ManuallyDrop<DetachedReservation>,
}
// Explicit owner ensures the weak reservation outlives the physical allocation.
struct RegistryAllocation(std::ptr::NonNull<RegistryPage>);
unsafe impl Send for RegistryAllocation {}
impl std::ops::Deref for RegistryAllocation {
    type Target = RegistryPage;
    fn deref(&self) -> &RegistryPage {
        unsafe { self.0.as_ref() }
    }
}
impl std::ops::DerefMut for RegistryAllocation {
    fn deref_mut(&mut self) -> &mut RegistryPage {
        unsafe { self.0.as_mut() }
    }
}
impl Drop for RegistryAllocation {
    fn drop(&mut self) {
        struct Release {
            pointer: std::ptr::NonNull<RegistryPage>,
            charge: Option<DetachedReservation>,
        }
        impl Drop for Release {
            fn drop(&mut self) {
                unsafe {
                    std::alloc::dealloc(
                        self.pointer.as_ptr().cast(),
                        std::alloc::Layout::new::<RegistryPage>(),
                    );
                }
                drop(self.charge.take());
            }
        }
        unsafe {
            let release = Release {
                pointer: self.0,
                charge: Some(std::mem::ManuallyDrop::take(&mut self.0.as_mut()._charge)),
            };
            std::ptr::drop_in_place(self.0.as_ptr());
            drop(release);
        }
    }
}
#[derive(Default)]
struct ReclaimerRegistry {
    head: Option<RegistryAllocation>,
}
impl ReclaimerRegistry {
    fn insert(&mut self, item: &WeakReclaimer) -> Option<Option<WeakReclaimer>> {
        let mut page = self.head.as_deref_mut();
        while let Some(p) = page {
            for slot in &mut p.entries {
                if slot.as_ref().is_none_or(|w| w.strong_count() == 0) {
                    return Some(slot.replace(item.clone()));
                }
            }
            page = p.next.as_deref_mut();
        }
        None
    }
    fn iter(&self) -> RegistryIter<'_> {
        RegistryIter {
            page: self.head.as_deref(),
            index: 0,
        }
    }
}
impl Drop for ReclaimerRegistry {
    fn drop(&mut self) {
        let mut page = self.head.take();
        while let Some(mut p) = page {
            page = p.next.take();
        }
    }
}
struct RegistryIter<'a> {
    page: Option<&'a RegistryPage>,
    index: usize,
}
impl<'a> Iterator for RegistryIter<'a> {
    type Item = &'a WeakReclaimer;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let p = self.page?;
            if self.index == REGISTRY_PAGE_ENTRIES {
                self.page = p.next.as_deref();
                self.index = 0;
                continue;
            }
            let i = self.index;
            self.index += 1;
            if let Some(w) = &p.entries[i] {
                return Some(w);
            }
        }
    }
}
pub(crate) struct DetachedReservation {
    references: ReferenceCounts,
    budget: crate::storage::BudgetWeak,
    bytes: usize,
    live: usize,
    category: ResourceCategory,
}
impl DetachedReservation {
    pub(crate) fn owner_reference(&self, kind: OwnerKind, acquire: bool) {
        {
            let budget = self.budget.ledger();
            self.references.owner(&budget, self.bytes, kind, acquire);
        }
    }
}
impl Drop for DetachedReservation {
    fn drop(&mut self) {
        {
            let budget = self.budget.ledger();
            let mut s = budget.state.lock().unwrap();
            self.references.reweight(&mut s, self.bytes, 0);
            s.stats.current -= self.bytes as u64;
            let c = &mut s.detailed.resources[self.category as usize];
            c.current -= self.bytes as u64;
            c.live -= self.live as u64;
            c.reserved = c.current - c.live;
            if self.bytes != 0 {
                budget.capacity_changed(&mut s);
            }
        }
    }
}
/// Every retained storage reference has one explicit purpose. Cache and lease
/// totals are unique per allocation, not sums of message ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum OwnerKind {
    Parser,
    Pending,
    Operation,
    Cache,
    Lease,
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnershipStatistics {
    pub bytes: [u64; 5],
    pub immediately_reclaimable: u64,
    pub externally_releasable: u64,
}
#[derive(Default)]
struct ReferenceCounts {
    owners: [std::sync::atomic::AtomicUsize; 5],
    leases: std::sync::atomic::AtomicUsize,
    caches: std::sync::atomic::AtomicUsize,
}
impl ReferenceCounts {
    // Called with the same ledger lock as the capacity update. Ownership counts
    // stay attached to storage when its reservation is resized or detached.
    fn reweight(&self, state: &mut State, before: usize, after: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let owners: [usize; 5] = std::array::from_fn(|i| self.owners[i].load(Relaxed));
        let update = |total: &mut u64, active: bool| {
            if active {
                *total = *total - before as u64 + after as u64;
            }
        };
        for (total, count) in state.ownership.bytes.iter_mut().zip(owners) {
            update(total, count != 0);
        }
        let internal = owners[0..3].iter().any(|n| *n != 0);
        update(
            &mut state.ownership.immediately_reclaimable,
            owners[3] != 0 && !internal && owners[4] == 0,
        );
        update(
            &mut state.ownership.externally_releasable,
            owners[4] != 0 && !internal,
        );
        update(
            &mut state.detailed.cache_payload_bytes,
            self.caches.load(Relaxed) != 0,
        );
        update(
            &mut state.detailed.lease_payload_bytes,
            self.leases.load(Relaxed) != 0,
        );
    }
    fn owner(&self, budget: &MemoryBudget, bytes: usize, kind: OwnerKind, acquire: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        let mut state = budget.state.lock().unwrap();
        let before: [usize; 5] = std::array::from_fn(|i| self.owners[i].load(Relaxed));
        let mut after = before;
        let n = &mut after[kind as usize];
        *n = if acquire {
            n.checked_add(1).expect("Owner count overflow")
        } else {
            n.checked_sub(1).expect("Unbalanced storage ownership")
        };
        self.owners[kind as usize].store(*n, Relaxed);
        let delta = |total: &mut u64, old: bool, new: bool| {
            if old && !new {
                *total -= bytes as u64;
            }
            if !old && new {
                *total += bytes as u64;
            }
        };
        for i in 0..5 {
            delta(&mut state.ownership.bytes[i], before[i] != 0, after[i] != 0);
        }
        let cache_only = |c: &[usize; 5]| c[3] != 0 && c[0..3].iter().all(|n| *n == 0) && c[4] == 0;
        let external = |c: &[usize; 5]| c[4] != 0 && c[0..3].iter().all(|n| *n == 0);
        delta(
            &mut state.ownership.immediately_reclaimable,
            cache_only(&before),
            cache_only(&after),
        );
        delta(
            &mut state.ownership.externally_releasable,
            external(&before),
            external(&after),
        );
        if !acquire || external(&before) && !external(&after) || cache_only(&before) && !cache_only(&after) {
            budget.capacity_changed(&mut state);
        }
    }
    fn update(&self, budget: &MemoryBudget, bytes: usize, cache: bool, acquire: bool) {
        let mut state = budget.state.lock().unwrap();
        let counter = if cache { &self.caches } else { &self.leases };
        let value = if acquire {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
        };
        let total = if cache {
            &mut state.detailed.cache_payload_bytes
        } else {
            &mut state.detailed.lease_payload_bytes
        };
        if acquire && value == 0 {
            *total += bytes as u64;
        } else if !acquire && value == 1 {
            *total -= bytes as u64;
        }
    }
}
pub trait SharedSource: AsRef<[u8]> + Send + Sync {
    fn owner_reference(&self, _kind: OwnerKind, _acquire: bool) {}
    fn reference(&self, _cache: bool, _acquire: bool) {}
}
impl SharedSource for Vec<u8> {}
/// One mapping owner in one domain. Cloning the mapping owner does not clone
/// this registration; it remains live until the last mapping reference is gone.
pub struct MappingRegistration {
    domain: crate::storage::BudgetRef,
    length: u64,
}
impl MappingRegistration {
    pub fn belongs_to(&self, domain: &crate::storage::BudgetRef) -> bool {
        crate::storage::BudgetRef::ptr_eq(&self.domain, domain)
    }
}
impl Drop for MappingRegistration {
    fn drop(&mut self) {
        self.domain.state.lock().unwrap().mapped_logical_bytes -= self.length;
    }
}
pub struct MemoryBudget {
    #[cfg(feature = "allocation-audit")]
    allocation_observer: std::sync::atomic::AtomicUsize,
    limits: BudgetLimits,
    state: Mutex<State>,
    leases: std::sync::atomic::AtomicUsize,
    reclaimers: Mutex<ReclaimerRegistry>,
    clock: std::sync::atomic::AtomicU64,
    capacity_version: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "allocation-audit"))]
    allocation_attempts: std::sync::atomic::AtomicUsize,
    #[cfg(any(test, feature = "allocation-audit"))]
    fail_allocation: std::sync::atomic::AtomicUsize,
}
impl std::fmt::Debug for MemoryBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryBudget")
            .field("limits", &self.limits)
            .finish()
    }
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self::new(BudgetLimits::default())
    }
}
impl MemoryBudget {
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            #[cfg(feature = "allocation-audit")]
            allocation_observer: std::sync::atomic::AtomicUsize::new(0),
            limits,
            state: Mutex::new(State::default()),
            leases: std::sync::atomic::AtomicUsize::new(0),
            reclaimers: Mutex::new(ReclaimerRegistry::default()),
            clock: std::sync::atomic::AtomicU64::new(1),
            capacity_version: std::sync::atomic::AtomicU64::new(0),
            #[cfg(any(test, feature = "allocation-audit"))]
            allocation_attempts: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "allocation-audit"))]
            fail_allocation: std::sync::atomic::AtomicUsize::new(usize::MAX),
        }
    }
    /// Test instrumentation only; never participates in production accounting.
    #[cfg(feature = "allocation-audit")]
    pub fn observe_allocations(&self, observer: fn(usize, usize, usize, usize, usize)) {
        self.allocation_observer
            .store(observer as usize, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn allocation_committed(
        &self,
        pointer: *mut u8,
        bytes: usize,
        category: ResourceCategory,
    ) {
        #[cfg(feature = "allocation-audit")]
        {
            let observer = self
                .allocation_observer
                .load(std::sync::atomic::Ordering::Acquire);
            if observer != 0 {
                let observer: fn(usize, usize, usize, usize, usize) =
                    unsafe { std::mem::transmute(observer) };
                observer(
                    self as *const Self as usize,
                    pointer as usize,
                    bytes,
                    category as usize,
                    0,
                );
            }
        }
        #[cfg(not(feature = "allocation-audit"))]
        let _ = (pointer, bytes, category);
    }
    pub(crate) fn allocation_reclassified(
        &self,
        pointer: *mut u8,
        bytes: usize,
        category: ResourceCategory,
    ) {
        #[cfg(feature = "allocation-audit")]
        {
            let observer = self
                .allocation_observer
                .load(std::sync::atomic::Ordering::Acquire);
            if observer != 0 {
                let observer: fn(usize, usize, usize, usize, usize) =
                    unsafe { std::mem::transmute(observer) };
                observer(
                    self as *const Self as usize,
                    pointer as usize,
                    bytes,
                    category as usize,
                    1,
                );
            }
        }
        #[cfg(not(feature = "allocation-audit"))]
        let _ = (pointer, bytes, category);
    }
    pub(crate) fn allocation_attempt(&self) -> std::io::Result<()> {
        #[cfg(any(test, feature = "allocation-audit"))]
        {
            let index = self
                .allocation_attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if index
                == self
                    .fail_allocation
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
        }
        Ok(())
    }
    #[cfg(any(test, feature = "allocation-audit"))]
    pub fn fail_allocation_at(&self, index: usize) {
        self.allocation_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.fail_allocation
            .store(index, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(any(test, feature = "allocation-audit"))]
    pub fn allocation_attempt_count(&self) -> usize {
        self.allocation_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn capacity_version(&self) -> u64 {
        self.capacity_version
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn capacity_changed(&self, state: &mut State) {
        state.waiters.release(state.available_capacity(self.limits.total), state.ownership.externally_releasable as usize, state.active_releases);
        self.capacity_version
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        if let Some((context, notify)) = state.capacity_observer { notify(context); }
    }
    /// Native bookkeeping only. Called under the ledger lock; the observer must
    /// not allocate, wait for a consumer, call managed code, or reenter this domain.
    /// Clearing it waits for any in-progress observer before returning.
    pub fn set_capacity_observer(&self, observer: Option<(usize, fn(usize))>) {
        self.state.lock().unwrap().capacity_observer = observer;
    }
    pub fn touch(&self) -> u64 {
        self.clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn prune_reclaimers(&self) {
        let count = match self.reclaimers.try_lock() {
            Ok(registry) => registry.iter().count(),
            Err(_) => return,
        };
        // Bounded snapshots also clear dead controls in partially occupied pages.
        // Their final weak drops may acquire the ledger, so retire outside all locks.
        for _ in 0..=count / REGISTRY_PAGE_ENTRIES {
            let Ok(mut registry) = self.reclaimers.try_lock() else {
                return;
            };
            let mut retired = ReclaimerRegistry::default();
            let mut controls: [Option<WeakReclaimer>; REGISTRY_PAGE_ENTRIES] =
                std::array::from_fn(|_| None);
            let mut used = 0;
            let mut link = &mut registry.head;
            while let Some(page) = link.as_mut() {
                for entry in &mut page.entries {
                    if used < REGISTRY_PAGE_ENTRIES
                        && entry.as_ref().is_some_and(|w| w.strong_count() == 0)
                    {
                        controls[used] = entry.take();
                        used += 1;
                    }
                }
                if page.entries.iter().all(Option::is_none) {
                    let mut page = link.take().unwrap();
                    *link = page.next.take();
                    page.next = retired.head.take();
                    retired.head = Some(page);
                } else {
                    link = &mut link.as_mut().unwrap().next;
                }
                if used == REGISTRY_PAGE_ENTRIES {
                    break;
                }
            }
            drop(registry);
            drop(controls);
            drop(retired);
            if used < REGISTRY_PAGE_ENTRIES {
                break;
            }
        }
    }
    fn reclaim_for(&self, additional: usize) {
        if self.release_idle_for(additional) {
            return;
        }
        // A concurrent reader disposal may have skipped pruning a busy registry.
        // Expired controls must not make the next allocation fail permanently.
        self.prune_reclaimers();
        if self.release_idle_for(additional) {
            return;
        }
        let count = self.reclaimers.lock().unwrap().iter().count();
        let mut previous = 0;
        for _ in 0..count {
            let mut candidate: Option<crate::charged::weak::ReclaimerPin> = None;
            // Clone only weak controls under the registry lock. Upgrades and all
            // strong/weak destruction happen outside it, including losing candidates.
            for start in (0..count).step_by(REGISTRY_PAGE_ENTRIES) {
                let mut snapshot: [Option<WeakReclaimer>; REGISTRY_PAGE_ENTRIES] =
                    std::array::from_fn(|_| None);
                {
                    let registry = self.reclaimers.lock().unwrap();
                    for (slot, entry) in snapshot.iter_mut().zip(registry.iter().skip(start)) {
                        *slot = Some(entry.clone());
                    }
                }
                for weak in snapshot.into_iter().flatten() {
                    let Some(pin) = weak.upgrade() else {
                        continue;
                    };
                    let touched = pin.last_access();
                    if touched > previous
                        && candidate.as_ref().is_none_or(|c| touched < c.last_access())
                    {
                        candidate = Some(pin);
                    }
                }
            }
            let Some(candidate) = candidate else {
                break;
            };
            previous = candidate.last_access();
            candidate.reclaim();
            // Eviction can transfer shared blocks to the idle pool.
            if self.release_idle_for(additional) {
                break;
            }
        }
    }
    pub fn acquire_lease(&self) {
        self.leases
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub fn release_lease(&self) {
        self.leases
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        self.capacity_changed(&mut self.state.lock().unwrap());
    }
    pub fn lease_count(&self) -> usize {
        self.leases.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn has_leases(&self) -> bool {
        self.leases.load(std::sync::atomic::Ordering::Acquire) != 0
    }

    pub fn mapped_logical_bytes(&self) -> u64 {
        self.state.lock().unwrap().mapped_logical_bytes
    }
    pub fn ownership_statistics(&self) -> OwnershipStatistics {
        let state = self.state.lock().unwrap();
        let mut result = state.ownership;
        result.immediately_reclaimable += state.stats.retained;
        result
    }
    pub fn statistics(&self) -> BudgetStatistics {
        self.state.lock().unwrap().stats
    }
    pub fn detailed_statistics(&self) -> DetailedStatistics {
        let state = self.state.lock().unwrap();
        let mut result = state.detailed;
        result.current_bytes = state.stats.current;
        result.peak_bytes = state.stats.peak;
        result.idle_bytes = state.stats.retained;
        result.immediately_reclaimable_bytes =
            state.ownership.immediately_reclaimable + state.stats.retained;
        result.mapped_logical_bytes = state.mapped_logical_bytes;
        result
    }
    pub fn reallocated(&self) {
        self.state.lock().unwrap().detailed.reallocation_count += 1;
    }
    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }
    pub fn copied(&self, n: usize) {
        self.copy_bytes(CopyKind::Other, n);
    }
    pub fn copy_bytes(&self, kind: CopyKind, n: usize) {
        let mut s = self.state.lock().unwrap();
        if !matches!(kind, CopyKind::Delivery) {
            s.stats.copied += n as u64;
        }
        let f = &mut s.detailed.flow;
        match kind {
            CopyKind::Input => f.input_copy += n as u64,
            CopyKind::Compaction => f.compaction_copy += n as u64,
            CopyKind::Delivery => f.delivery_copy += n as u64,
            CopyKind::Other => f.other_copy += n as u64,
        }
    }
    pub fn codec_bytes(&self, encode: bool, input: usize, output: usize) {
        let mut s = self.state.lock().unwrap();
        let f = &mut s.detailed.flow;
        if encode {
            f.encoded_input += input as u64;
            f.encoded_output += output as u64;
        } else {
            f.decoded_input += input as u64;
            f.decoded_output += output as u64;
        }
    }
    pub fn decode_event(&self, complete: bool) {
        let mut s = self.state.lock().unwrap();
        if complete {
            s.detailed.flow.decode_completed += 1;
        } else {
            s.detailed.flow.decode_started += 1;
        }
    }
    pub fn cache_event(&self, kind: u8) {
        let mut s = self.state.lock().unwrap();
        match kind {
            0 => s.detailed.flow.cache_misses += 1,
            1 => s.detailed.flow.cache_hits += 1,
            _ => s.detailed.flow.cache_evictions += 1,
        }
    }


}

impl crate::storage::BudgetRef {
    pub fn register_reclaimer(&self, item: WeakReclaimer) -> std::io::Result<()> {
        self.register_reclaimer_fixed(item).map_err(StorageFailure::into_io)
    }
    pub fn register_reclaimer_fixed(&self, item: WeakReclaimer) -> Result<(), StorageFailure> {
        {
            let mut registry = self.reclaimers.lock().unwrap();
            if let Some(retired) = registry.insert(&item) {
                drop(registry);
                drop(retired);
                return Ok(());
            }
        }
        // Allocate outside the registry lock: reserving may reclaim another cache.
        let layout = std::alloc::Layout::new::<RegistryPage>();
        let mut charge = self.reserve_class_fixed(layout.size(), ResourceCategory::Scratch)
            .map_err(StorageFailure::from)?;
        let failure = || StorageFailure::at(self, ResourceCategory::Scratch, layout.size(),
            "reclaimer registry allocation", StorageFailureKind::SystemAllocation);
        self.allocation_attempt().map_err(|_| failure())?;
        let raw = unsafe { std::alloc::alloc(layout) }.cast::<RegistryPage>();
        if raw.is_null() {
            return Err(failure());
        }
        charge.commit(layout.size());
        self.allocation_committed(raw.cast(), layout.size(), ResourceCategory::Scratch);
        let mut page = unsafe {
            raw.write(RegistryPage {
                entries: std::array::from_fn(|_| None),
                next: None,
                _charge: std::mem::ManuallyDrop::new(charge.detach()),
            });
            RegistryAllocation(std::ptr::NonNull::new_unchecked(raw))
        };
        let mut registry = self.reclaimers.lock().unwrap();
        if let Some(retired) = registry.insert(&item) {
            drop(registry);
            drop(retired);
            drop(page);
            return Ok(());
        }
        page.entries[0] = Some(item);
        page.next = registry.head.take();
        registry.head = Some(page);
        Ok(())
    }
    pub fn register_mapping(&self, length: u64) -> std::io::Result<MappingRegistration> {
        let mut state = self.state.lock().unwrap();
        state.mapped_logical_bytes = state
            .mapped_logical_bytes
            .checked_add(length)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(MappingRegistration {
            domain: self.clone(),
            length,
        })
    }
    /// Reserve capacity without allocating an error when the domain is exhausted.
    pub fn try_reserve(&self, bytes:usize, category:ResourceCategory)->Result<Reservation,StorageFailure> {
        self.reserve_class_fixed(bytes,category).map_err(StorageFailure::from)
    }
    pub fn reserve(&self, bytes: usize) -> std::io::Result<Reservation> {
        self.reserve_class(bytes, ResourceCategory::Scratch)
    }
    pub fn reserve_class(
        &self,
        bytes: usize,
        category: ResourceCategory,
    ) -> std::io::Result<Reservation> {
        self.reserve_class_fixed(bytes, category).map_err(ReservationFailure::into_io)
    }
    pub(crate) fn reserve_class_fixed(
        &self,
        bytes: usize,
        category: ResourceCategory,
    ) -> Result<Reservation, ReservationFailure> {
        let mut r = Reservation {
            budget: self.clone(),
            bytes: 0,
            live: 0,
            category,
            references: ReferenceCounts::default(),
        };
        r.resize_fixed(bytes)?;
        Ok(r)
    }
}

pub struct Reservation {
    references: ReferenceCounts,
    budget: crate::storage::BudgetRef,
    bytes: usize,
    live: usize,
    category: ResourceCategory,
}
impl Reservation {
    pub fn owner_reference(&self, kind: OwnerKind, acquire: bool) {
        self.references
            .owner(&self.budget, self.bytes, kind, acquire);
    }
    pub fn reference(&self, cache: bool, acquire: bool) {
        self.references
            .update(&self.budget, self.bytes, cache, acquire);
    }
    pub(crate) fn detach(mut self) -> DetachedReservation {
        let result = DetachedReservation {
            references: std::mem::take(&mut self.references),
            budget: crate::storage::BudgetRef::downgrade(&self.budget),
            bytes: self.bytes,
            live: self.live,
            category: self.category,
        };
        self.bytes = 0;
        self.live = 0;
        result
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn limits(&self) -> BudgetLimits {
        self.budget.limits
    }
    pub fn domain(&self) -> crate::storage::BudgetRef {
        self.budget.clone()
    }
    pub fn commit(&mut self, live: usize) {
        assert!(live <= self.bytes);
        let mut s = self.budget.state.lock().unwrap();
        let c = &mut s.detailed.resources[self.category as usize];
        c.live = c.live - self.live as u64 + live as u64;
        c.reserved = c.current - c.live;
        if live > self.live {
            s.detailed.allocation_count += 1;
            s.detailed.allocated_bytes += (live - self.live) as u64;
        }
        self.live = live;
    }
    pub fn resize(&mut self, bytes: usize) -> std::io::Result<()> {
        self.resize_fixed(bytes).map_err(ReservationFailure::into_io)
    }
    pub(crate) fn resize_fixed(&mut self, bytes: usize) -> Result<(), ReservationFailure> {
        if bytes > self.bytes {
            self.budget.reclaim_for(bytes - self.bytes);
        }
        let mut s = self.budget.state.lock().unwrap();
        if (s.stats.current - self.bytes as u64).checked_add(bytes as u64)
            .is_none_or(|required| required > self.budget.limits.total as u64) {
            s.detailed.rejected += 1;
            return Err(ReservationFailure {
                kind: if bytes > self.budget.limits.total.saturating_sub(BudgetRef::allocation_size()) {
                    std::io::ErrorKind::Other
                } else {
                    std::io::ErrorKind::WouldBlock
                },
                limit: StorageLimit {
                    resource: match self.category {
                        ResourceCategory::CodecEncoder => "CodecEncoder",
                        ResourceCategory::CodecDecoder => "CodecDecoder",
                        _ => "NativeDomain",
                    },
                    limit: self.budget.limits.total,
                    domain_limit: self.budget.limits.total,
                    requested: bytes.saturating_sub(self.bytes),
                    current: s.stats.current as usize,
                    phase: "reservation",
                },
            });
        }
        self.references.reweight(&mut s, self.bytes, bytes);
        s.stats.current = s.stats.current - self.bytes as u64 + bytes as u64;
        s.stats.peak = s.stats.peak.max(s.stats.current);
        if bytes > self.bytes {
            s.stats.allocations += 1;
        }
        let c = &mut s.detailed.resources[self.category as usize];
        c.current = c.current - self.bytes as u64 + bytes as u64;
        c.peak = c.peak.max(c.current);
        if self.live > bytes {
            c.live -= (self.live - bytes) as u64;
            self.live = bytes;
        }
        c.reserved = c.current - c.live;
        if bytes < self.bytes {
            self.budget.capacity_changed(&mut s);
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut s = self.budget.state.lock().unwrap();
        self.references.reweight(&mut s, self.bytes, 0);
        s.stats.current -= self.bytes as u64;
        let c = &mut s.detailed.resources[self.category as usize];
        c.current -= self.bytes as u64;
        c.live -= self.live as u64;
        c.reserved = c.current - c.live;
        if self.bytes != 0 {
            self.budget.capacity_changed(&mut s);
        }
    }
}

/// Shared conservative reservation for declaration/index containers. Equality deliberately
/// ignores resource ownership so Summary retains its semantic equality contract.
pub(crate) struct Bookkeeping {
    allocation: Option<crate::charged::ChargedShared<Mutex<Reservation>>>,
    owner: OwnerKind,
}
impl Default for Bookkeeping {
    fn default() -> Self {
        Self {
            allocation: None,
            owner: OwnerKind::Parser,
        }
    }
}
impl Clone for Bookkeeping {
    fn clone(&self) -> Self {
        let result = Self {
            allocation: self.allocation.clone(),
            owner: self.owner,
        };
        result.reference(true);
        result
    }
}
impl Drop for Bookkeeping {
    fn drop(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            let release = allocation.begin_capacity_release();
            allocation.charge_owner(self.owner, false);
            allocation.lock().unwrap_or_else(|e| e.into_inner())
                .owner_reference(self.owner, false);
            drop(allocation);
            drop(release);
        }
    }
}
impl PartialEq for Bookkeeping {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Bookkeeping {}
impl Bookkeeping {
    pub fn is_charged(&self) -> bool {
        self.allocation.is_some()
    }
    pub fn bytes(&self) -> usize {
        self.allocation
            .as_ref()
            .map_or(0, |r| r.lock().unwrap().bytes())
    }
    pub fn new(budget: &crate::storage::BudgetRef) -> std::io::Result<Self> {
        Self::new_owned(budget, OwnerKind::Parser)
    }
    pub fn new_owned(budget: &crate::storage::BudgetRef, owner: OwnerKind) -> std::io::Result<Self> {
        let allocation = crate::charged::ChargedShared::new(
            Mutex::new(budget.reserve_class(0, ResourceCategory::Declaration)?),
            budget,
            ResourceCategory::Declaration,
        )?;
        let result = Self {
            allocation: Some(allocation),
            owner,
        };
        result.reference(true);
        Ok(result)
    }
    fn reference(&self, acquire: bool) {
        if let Some(allocation) = &self.allocation {
            allocation.charge_owner(self.owner, acquire);
            // Cleanup must not panic again after a failed mutation poisoned
            // this mutex. Reservation retains its own rollback state.
            allocation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .owner_reference(self.owner, acquire);
        }
    }
    pub fn grow(&self, bytes: usize) -> std::io::Result<()> {
        if let Some(r) = &self.allocation {
            let mut r = r.lock().unwrap();
            let n = r
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| std::io::Error::other("Bookkeeping capacity overflow"))?;
            r.resize(n)?;
        }
        Ok(())
    }
}
#[derive(Clone)]
enum Backing {
    Empty,
    Owned(Allocation),
    External(Arc<dyn SharedSource>),
    ChargedExternal(crate::charged::ChargedSource),
}
pub struct SharedBytes {
    owner: OwnerKind,
    backing: Backing,
    range: Range<usize>,
}
impl Clone for SharedBytes {
    fn clone(&self) -> Self {
        self.clone_for(self.owner)
    }
}
impl Drop for SharedBytes {
    fn drop(&mut self) {
        let _release = match &self.backing {
            Backing::Owned(b) => Some(b.budget.begin_capacity_release()),
            Backing::ChargedExternal(b) => Some(b.begin_capacity_release()),
            Backing::External(_) | Backing::Empty => None,
        };
        self.owner_reference(self.owner, false);
        // Release the backing while the scope still protects the ownership gap.
        drop(std::mem::replace(&mut self.backing, Backing::Empty));
    }
}
impl SharedBytes {
    fn new(backing: Backing, range: Range<usize>, owner: OwnerKind) -> Self {
        let result = Self {
            backing,
            range,
            owner,
        };
        result.owner_reference(owner, true);
        result
    }
    pub fn clone_for(&self, owner: OwnerKind) -> Self {
        Self::new(self.backing.clone(), self.range.clone(), owner)
    }
    pub fn set_owner(&mut self, owner: OwnerKind) {
        if self.owner != owner {
            // Acquire before releasing so a transition never appears unpinned.
            self.owner_reference(owner, true);
            self.owner_reference(self.owner, false);
            self.owner = owner;
        }
    }

    pub fn lease_reference(&self, acquire: bool) {
        SharedSource::reference(self, false, acquire);
    }
    pub fn cache_reference(&self, acquire: bool) {
        SharedSource::reference(self, true, acquire);
    }
    /// Capacity charged for pooled storage; external mappings have no native heap capacity.
    pub fn owned_capacity(&self) -> Option<usize> {
        match &self.backing {
            Backing::Owned(b) => Some(b.length),
            Backing::External(_) | Backing::ChargedExternal(_) | Backing::Empty => None,
        }
    }
    pub fn empty() -> Self {
        Self::new(Backing::Empty, 0..0, OwnerKind::Operation)
    }
    pub fn charged_external(source: crate::charged::ChargedSource, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= source.as_ref().len());
        Self::new(
            Backing::ChargedExternal(source),
            range,
            OwnerKind::Operation,
        )
    }
    pub fn external(source: Arc<dyn SharedSource>, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= source.as_ref().as_ref().len());
        Self::new(Backing::External(source), range, OwnerKind::Operation)
    }
    pub fn slice(&self, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= self.range.len());
        Self::new(
            self.backing.clone(),
            self.range.start + range.start..self.range.start + range.end,
            self.owner,
        )
    }
}
impl SharedSource for SharedBytes {
    fn owner_reference(&self, kind: OwnerKind, acquire: bool) {
        match &self.backing {
            Backing::Owned(b) => b
                .references
                .owner(&b.budget, b.charged_bytes(), kind, acquire),
            Backing::External(b) => b.owner_reference(kind, acquire),
            Backing::ChargedExternal(b) => b.owner_reference(kind, acquire),
            Backing::Empty => {}
        }
    }
    fn reference(&self, cache: bool, acquire: bool) {
        match &self.backing {
            Backing::Owned(b) => b.references.update(&b.budget, b.length, cache, acquire),
            Backing::External(b) => b.reference(cache, acquire),
            Backing::ChargedExternal(b) => b.reference(cache, acquire),
            Backing::Empty => {}
        }
    }
}
impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(b) => unsafe {
                std::slice::from_raw_parts(b.pointer.add(self.range.start), self.range.len())
            },
            Backing::External(b) => &b.as_ref().as_ref()[self.range.clone()],
            Backing::ChargedExternal(b) => &b.as_ref()[self.range.clone()],
            Backing::Empty => &[],
        }
    }
}

pub(crate) struct WriteBuffer {
    backing: Option<SharedBytes>,
    read_only: bool,
    pub budget: crate::storage::BudgetRef,
    pub category: ResourceCategory,
}
impl Default for WriteBuffer {
    fn default() -> Self {
        Self {
            backing: None,
            read_only: false,
            budget: crate::storage::BudgetRef::new(Default::default()).unwrap(),
            category: ResourceCategory::Input,
        }
    }
}
impl WriteBuffer {
    pub fn with_budget(budget: crate::storage::BudgetRef, category: ResourceCategory) -> Self {
        Self { backing: None, read_only: false, budget, category }
    }
    pub fn bytes(&self) -> &[u8] {
        self.backing.as_ref().map_or(&[], |b| b.as_ref())
    }
    pub fn external(&mut self, bytes: SharedBytes) {
        // Preserve a subrange without exposing bytes outside it.
        let mut bytes = bytes;
        bytes.set_owner(OwnerKind::Parser);
        self.backing = Some(bytes);
        self.read_only = true;
    }
    pub fn reserve(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        self.reserve_fixed(size,preserve).map_err(StorageFailure::into_io)
    }
    pub fn reserve_fixed(&mut self,size:usize,preserve:Range<usize>)->Result<(),StorageFailure> {
        if let Some(SharedBytes {
            backing: Backing::Owned(b),
            ..
        }) = &self.backing
        {
            if !self.read_only && b.bytes().len() >= size {
                return Ok(());
            }
        }
        let growing = matches!(&self.backing, Some(SharedBytes { backing: Backing::Owned(old), .. }) if size > old.length);
        let allocation = self.budget.allocate_fixed(size, self.category)?;
        let old = &self.bytes()[preserve.clone()];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copy_bytes(CopyKind::Compaction, old.len());
        let length = allocation.length;
        self.read_only = false;
        self.backing = Some(SharedBytes::new(
            Backing::Owned(allocation),
            0..length,
            OwnerKind::Parser,
        ));
        if growing {
            self.budget.reallocated();
        }
        Ok(())
    }
    pub fn relocate(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        self.relocate_fixed(size, preserve).map_err(StorageFailure::into_io)
    }
    pub fn relocate_fixed(&mut self, size: usize, preserve: Range<usize>) -> Result<(), StorageFailure> {
        let growing = matches!(&self.backing, Some(SharedBytes { backing: Backing::Owned(old), .. }) if size > old.length);
        let allocation = self.budget.allocate_fixed(size, self.category)?;
        let old = &self.bytes()[preserve];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copy_bytes(CopyKind::Compaction, old.len());
        let length = allocation.length;
        self.read_only = false;
        self.backing = Some(SharedBytes::new(
            Backing::Owned(allocation),
            0..length,
            OwnerKind::Parser,
        ));
        if growing {
            self.budget.reallocated();
        }
        Ok(())
    }
    pub fn reset(&mut self) {
        if self.read_only
            || !matches!(&self.backing, Some(SharedBytes { backing: Backing::Owned(b), .. }) if b.unique())
        {
            self.backing = None;
        }
    }
    pub fn exclusive(&self) -> bool {
        !self.read_only
            && matches!(&self.backing, Some(SharedBytes { backing: Backing::Owned(b), .. }) if b.unique())
    }
    pub unsafe fn writable(&mut self, range: Range<usize>) -> &mut [u8] {
        assert!(!self.read_only);
        match &self.backing.as_ref().unwrap().backing {
            Backing::Owned(b) => b.tail(range),
            _ => unreachable!(),
        }
    }
    pub fn share(&self, range: Range<usize>) -> SharedBytes {
        let bytes = self.backing.as_ref().unwrap();
        let mut result = bytes.slice(range);
        result.set_owner(OwnerKind::Operation);
        result
    }
}

#[cfg(test)]
mod capacity_version_tests {
    use super::*;
    #[test]
    fn detects_release_even_when_capacity_is_immediately_reoccupied() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let first = domain
            .reserve_class(1024, ResourceCategory::Scratch)
            .unwrap();
        let version = domain.capacity_version();
        drop(first);
        let _second = domain
            .reserve_class(1024, ResourceCategory::Scratch)
            .unwrap();
        assert_eq!(domain.workload_statistics().current, 1024);
        assert!(domain.capacity_version() > version);
        let version = domain.capacity_version();
        drop(domain.reserve_class(0, ResourceCategory::Scratch).unwrap());
        assert_eq!(domain.capacity_version(), version);
    }
}

#[cfg(test)]
mod registry_page_tests {
    use super::*;
    struct Item;
    impl Reclaimable for Item {
        fn last_access(&self) -> u64 {
            1
        }
        fn reclaim(&self) -> bool {
            false
        }
    }
    #[test]
    fn pressure_prunes_expired_registration_controls() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (16384) + crate::storage::BudgetRef::allocation_size(),
            block: 16384,
            retained: 0,
        }).unwrap();
        let item = crate::charged::weak::BudgetedArc::new(Item, &domain, ResourceCategory::Scratch)
            .unwrap();
        domain.register_reclaimer(item.reclaimer()).unwrap();
        drop(item);
        assert!(domain.workload_statistics().current > 0);
        let allocation = domain
            .reserve_class(16384, ResourceCategory::Scratch)
            .unwrap();
        assert_eq!(domain.workload_statistics().current, 16384);
        drop(allocation);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn pruning_releases_dead_controls_from_partially_live_pages() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut items: Vec<_> = (0..130)
            .map(|_| {
                crate::charged::weak::BudgetedArc::new(Item, &domain, ResourceCategory::Scratch)
                    .unwrap()
            })
            .collect();
        for item in &items {
            domain.register_reclaimer(item.reclaimer()).unwrap();
        }
        let survivor = items.pop().unwrap();
        drop(items);
        domain.prune_reclaimers();
        assert_eq!(
            domain.workload_statistics().current,
            (std::mem::size_of::<RegistryPage>() + survivor.allocation_bytes()) as u64
        );
        drop(survivor);
        domain.prune_reclaimers();
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn registration_pages_release_without_domain_cycle() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let weak = crate::storage::BudgetRef::downgrade(&domain);
        let items: Vec<_> = (0..130)
            .map(|_| {
                crate::charged::weak::BudgetedArc::new(Item, &domain, ResourceCategory::Scratch)
                    .unwrap()
            })
            .collect();
        for item in &items {
            domain.register_reclaimer(item.reclaimer()).unwrap();
        }
        assert_eq!(
            domain.workload_statistics().current,
            3 * std::mem::size_of::<RegistryPage>() as u64
                + items
                    .iter()
                    .map(|i| i.allocation_bytes() as u64)
                    .sum::<u64>()
        );
        drop(items);
        domain.prune_reclaimers();
        assert_eq!(domain.workload_statistics().current, 0);
        drop(domain);
        assert!(weak.upgrade().is_none());
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    fn domain() -> crate::storage::BudgetRef {
        crate::storage::BudgetRef::new(BudgetLimits {
            retained: 0,
            ..Default::default()
        }).unwrap()
    }
    #[test]
    fn empty_storage_supports_slices_and_owner_transitions_without_backing() {
        let mut empty = SharedBytes::empty();
        empty.set_owner(OwnerKind::Parser);
        let lease = empty.clone_for(OwnerKind::Lease);
        let slice = lease.slice(0..0);
        assert!(matches!(slice.backing, Backing::Empty));
        assert!(slice.as_ref().is_empty());
        assert_eq!(slice.owned_capacity(), None);
        lease.lease_reference(true);
        lease.lease_reference(false);
        drop(empty);
        assert!(lease.as_ref().is_empty());
    }
    #[test]
    fn reservation_growth_detach_and_release_preserve_owner_capacity() {
        let budget = domain();
        let mut charge = budget
            .reserve_class(128, ResourceCategory::Scratch)
            .unwrap();
        charge.owner_reference(OwnerKind::Cache, true);
        charge.reference(true, true);
        charge.resize(256).unwrap();
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 256);
        assert_eq!(budget.workload_detailed_statistics().cache_payload_bytes, 256);
        let charge = charge.detach();
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 256);
        charge.owner_reference(OwnerKind::Operation, true);
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 0);
        charge.owner_reference(OwnerKind::Operation, false);
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 256);
        drop(charge);
        assert_eq!(budget.ownership_statistics(), Default::default());
        assert_eq!(budget.workload_detailed_statistics().cache_payload_bytes, 0);
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn refused_growth_does_not_change_owner_capacity() {
        let budget = crate::storage::BudgetRef::new(BudgetLimits {
            total: (256) + crate::storage::BudgetRef::allocation_size(),
            block: 256,
            retained: 0,
        }).unwrap();
        let mut charge = budget
            .reserve_class(128, ResourceCategory::Scratch)
            .unwrap();
        charge.owner_reference(OwnerKind::Lease, true);
        charge.reference(false, true);
        assert!(charge.resize(257).is_err());
        assert_eq!(budget.ownership_statistics().externally_releasable, 128);
        assert_eq!(budget.workload_detailed_statistics().lease_payload_bytes, 128);
        charge.resize(64).unwrap();
        assert_eq!(budget.ownership_statistics().externally_releasable, 64);
        assert_eq!(budget.workload_detailed_statistics().lease_payload_bytes, 64);
        drop(charge);
        assert_eq!(budget.ownership_statistics(), Default::default());
        assert_eq!(budget.workload_detailed_statistics().lease_payload_bytes, 0);
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn mapping_length_overflow_does_not_publish_registration() {
        let budget = domain();
        let registration = budget.register_mapping(u64::MAX).unwrap();
        assert!(budget.register_mapping(1).is_err());
        assert_eq!(budget.mapped_logical_bytes(), u64::MAX);
        drop(registration);
        assert_eq!(budget.mapped_logical_bytes(), 0);
    }
    #[test]
    fn owners_are_unique_and_only_unpinned_cache_is_reclaimable() {
        let budget = domain();
        let mut writer = WriteBuffer {
            budget: budget.clone(),
            ..Default::default()
        };
        writer.reserve(128, 0..0).unwrap();
        let mut cache = writer.share(0..128);
        cache.set_owner(OwnerKind::Cache);
        let second_cache = cache.clone();
        let capacity = budget.workload_statistics().current;
        assert_eq!(
            budget.ownership_statistics().bytes[OwnerKind::Cache as usize],
            capacity
        );
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 0);
        drop(writer);
        assert_eq!(
            budget.ownership_statistics().immediately_reclaimable,
            capacity
        );
        let lease = cache.clone_for(OwnerKind::Lease);
        let second_lease = lease.clone();
        assert_eq!(
            budget.ownership_statistics().bytes[OwnerKind::Lease as usize],
            capacity
        );
        assert_eq!(budget.ownership_statistics().immediately_reclaimable, 0);
        assert_eq!(
            budget.ownership_statistics().externally_releasable,
            capacity
        );
        let pending = cache.clone_for(OwnerKind::Pending);
        assert_eq!(budget.ownership_statistics().externally_releasable, 0);
        drop(pending);
        drop(lease);
        assert_eq!(
            budget.ownership_statistics().externally_releasable,
            capacity
        );
        drop(second_lease);
        assert_eq!(
            budget.ownership_statistics().immediately_reclaimable,
            capacity
        );
        drop(cache);
        assert_eq!(
            budget.ownership_statistics().immediately_reclaimable,
            capacity
        );
        drop(second_cache);
        assert_eq!(
            budget.ownership_statistics(),
            OwnershipStatistics::default()
        );
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn shared_input_preserves_range_and_parser_pin_without_nested_owner() {
        let budget = domain();
        let mut writer = WriteBuffer {
            budget: budget.clone(),
            ..Default::default()
        };
        writer.reserve(128, 0..0).unwrap();
        unsafe {
            writer.writable(4..8).copy_from_slice(&[1, 2, 3, 4]);
        }
        let slice = writer.share(4..8);
        let mut reader = WriteBuffer {
            budget: budget.clone(),
            ..Default::default()
        };
        reader.external(slice);
        drop(writer);
        assert_eq!(reader.bytes(), &[1, 2, 3, 4]);
        let output = reader.share(1..3);
        assert_eq!(output.as_ref(), &[2, 3]);
        reader.reserve(8, 0..4).unwrap();
        unsafe {
            reader.writable(0..4).fill(9);
        }
        assert_eq!(output.as_ref(), &[2, 3]);
        drop(reader);
        assert_eq!(
            budget.ownership_statistics().bytes[OwnerKind::Parser as usize],
            0
        );
        drop(output);
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn concurrent_reference_lifetimes_leave_no_ownership_charge() {
        let budget = domain();
        let mut writer = WriteBuffer {
            budget: budget.clone(),
            ..Default::default()
        };
        writer.reserve(1, 0..0).unwrap();
        let shared = writer.share(0..1).clone_for(OwnerKind::Lease);
        drop(writer);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let shared = &shared;
                scope.spawn(move || {
                    for _ in 0..1000 {
                        let _pin = shared.clone_for(OwnerKind::Operation);
                    }
                });
            }
        });
        assert!(budget.ownership_statistics().externally_releasable > 0);
        drop(shared);
        assert_eq!(
            budget.ownership_statistics(),
            OwnershipStatistics::default()
        );
    }
}

#[cfg(test)]
mod reallocation_tests {
    use super::*;
    #[test]
    fn count_only_successful_growth_of_existing_storage() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (1 << 20) + crate::storage::BudgetRef::allocation_size(),
            block: 1 << 20,
            retained: 0,
        }).unwrap();
        let mut buffer = WriteBuffer {
            budget: domain.clone(),
            ..Default::default()
        };
        buffer.reserve(128, 0..0).unwrap();
        unsafe {
            buffer.writable(0..1)[0] = 42;
        }
        assert_eq!(domain.workload_detailed_statistics().reallocation_count, 0);
        let size = buffer.bytes().len();
        buffer.reserve(size + 1, 0..1).unwrap();
        assert_eq!(buffer.bytes()[0], 42);
        assert_eq!(domain.workload_detailed_statistics().reallocation_count, 1);
        let size = buffer.bytes().len();
        buffer.relocate(size, 0..1).unwrap();
        assert_eq!(domain.workload_detailed_statistics().reallocation_count, 1);
        let before = domain.workload_statistics().current;
        domain.fail_allocation_at(0);
        assert!(buffer.reserve(size + 1, 0..1).is_err());
        assert_eq!(domain.workload_detailed_statistics().reallocation_count, 1);
        assert_eq!(domain.workload_statistics().current, before);
        assert_eq!(buffer.bytes()[0], 42);
        drop(buffer);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

#[cfg(test)]
mod bookkeeping_control_tests {
    use super::*;
    #[test]
    fn poisoned_control_can_release_shared_owners_without_double_panic() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = Bookkeeping::new(&domain).unwrap();
        value.grow(1024).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = value.allocation.as_ref().unwrap().lock().unwrap();
            panic!("injected control mutation panic");
        })).is_err());
        let other = value.clone();
        drop(value);
        drop(other);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn exact_shared_control_and_reserved_capacity_have_independent_lifetimes() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let bookkeeping = Bookkeeping::new(&domain).unwrap();
        let control = domain.workload_statistics().current;
        assert!(control > 0);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 1);
        assert_eq!(
            domain.workload_detailed_statistics().resources[ResourceCategory::Declaration as usize].live,
            control
        );
        bookkeeping.grow(1024).unwrap();
        let other = bookkeeping.clone();
        assert_eq!(domain.workload_statistics().current, control + 1024);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            control + 1024
        );
        drop(bookkeeping);
        assert_eq!(other.bytes(), 1024);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 1);
        other.grow(256).unwrap();
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            control + 1280
        );
        drop(other);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn control_allocation_refusal_and_reservation_growth_preserve_ledger() {
        let probe = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = Bookkeeping::new(&probe).unwrap();
        let control = probe.workload_statistics().current as usize;
        drop(value);
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (control + 1024) + crate::storage::BudgetRef::allocation_size(),
            block: 64,
            retained: 0,
        }).unwrap();
        domain.fail_allocation_at(0);
        assert!(Bookkeeping::new(&domain).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        let value = Bookkeeping::new_owned(&domain, OwnerKind::Operation).unwrap();
        value.grow(1024).unwrap();
        assert!(value.grow(1).is_err());
        assert_eq!(value.bytes(), 1024);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            (control + 1024) as u64
        );
        drop(value);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
}

#[cfg(test)]
mod fixed_reservation_tests {
    use super::*;
    #[test]
    fn failed_growth_preserves_reservation_and_owner_until_retry() {
        let domain = BudgetRef::new(BudgetLimits {
            total: 8192 + BudgetRef::allocation_size(), block: 8192, retained: 0,
        }).unwrap();
        let occupied = domain.reserve_class_fixed(4096, ResourceCategory::Scratch).unwrap();
        let mut growing = domain.reserve_class_fixed(2048, ResourceCategory::CodecEncoder).unwrap();
        growing.owner_reference(OwnerKind::Operation, true);
        let current = domain.statistics().current;
        let owners = domain.ownership_statistics();
        let failure = growing.resize_fixed(6144).unwrap_err();
        assert_eq!(failure.kind, std::io::ErrorKind::WouldBlock);
        assert_eq!(failure.limit.resource, "CodecEncoder");
        assert_eq!(failure.limit.requested, 4096);
        assert_eq!(failure.limit.current, current as usize);
        assert_eq!(domain.statistics().current, current);
        assert_eq!(domain.ownership_statistics(), owners);
        let permanent = growing.resize_fixed(8193).unwrap_err();
        assert_eq!(permanent.kind, std::io::ErrorKind::Other);
        assert_eq!(permanent.limit.requested, 8193 - 2048);
        assert_eq!(domain.statistics().current, current);
        assert_eq!(domain.ownership_statistics(), owners);
        drop(occupied);
        growing.resize_fixed(6144).unwrap();
        assert_eq!(domain.workload_statistics().current, 6144);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Operation as usize], 6144);
        growing.owner_reference(OwnerKind::Operation, false);
        drop(growing);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
}
