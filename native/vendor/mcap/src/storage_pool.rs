//! One explicit allocation holds payload, shared header and idle linkage.
//! Idle blocks contain no strong domain reference, so the pool cannot retain its domain.
use super::*;
use std::{
    alloc::{alloc_zeroed, dealloc, Layout},
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};
const CLASSES: usize = usize::BITS as usize;
pub(super) struct Header {
    strong: AtomicUsize,
    pub(super) references: ReferenceCounts,
    pub(super) category: ResourceCategory,
    pub(super) pointer: *mut u8,
    pub(super) length: usize,
    layout: Layout,
    next: Option<NonNull<Header>>,
}
pub(super) struct IdlePool {
    heads: [Option<NonNull<Header>>; CLASSES],
}
// Access to every idle link is serialized by MemoryBudget::state.
unsafe impl Send for IdlePool {}
impl Default for IdlePool {
    fn default() -> Self {
        Self {
            heads: [None; CLASSES],
        }
    }
}
fn class(size: usize) -> usize {
    (usize::BITS - size.max(1).leading_zeros() - 1) as usize
}
impl IdlePool {
    fn push(&mut self, mut block: NonNull<Header>) {
        unsafe {
            let h = block.as_mut();
            let i = class(h.length);
            h.next = self.heads[i];
            self.heads[i] = Some(block);
        }
    }
    fn take(&mut self, minimum: usize) -> Option<NonNull<Header>> {
        for i in class(minimum)..CLASSES {
            let mut link = &mut self.heads[i];
            while let Some(mut block) = *link {
                unsafe {
                    let h = block.as_mut();
                    if h.length >= minimum {
                        *link = h.next.take();
                        return Some(block);
                    }
                    link = &mut h.next;
                }
            }
        }
        None
    }
    fn pop(&mut self) -> Option<NonNull<Header>> {
        self.take(0)
    }
}
unsafe fn free(block: NonNull<Header>) -> (usize, ResourceCategory) {
    let layout = block.as_ref().layout;
    let category = block.as_ref().category;
    std::ptr::drop_in_place(block.as_ptr());
    dealloc(block.as_ptr().cast(), layout);
    (layout.size(), category)
}
impl Drop for IdlePool {
    fn drop(&mut self) {
        while let Some(block) = self.pop() {
            unsafe {
                free(block);
            }
        }
    }
}
/// Private shared owner with a known header layout. No Rust Arc layout assumptions.
pub(super) struct Allocation {
    block: NonNull<Header>,
    pub(super) budget: crate::storage::BudgetRef,
}
unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}
impl std::ops::Deref for Allocation {
    type Target = Header;
    fn deref(&self) -> &Header {
        unsafe { self.block.as_ref() }
    }
}
impl Allocation {
    pub(super) fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.length) }
    }
    pub(super) unsafe fn tail(&self, range: Range<usize>) -> &mut [u8] {
        assert!(range.start <= range.end && range.end <= self.length);
        std::slice::from_raw_parts_mut(self.pointer.add(range.start), range.len())
    }
    pub(super) fn unique(&self) -> bool {
        self.strong.load(Ordering::Acquire) == 1
    }
    pub(super) fn charged_bytes(&self) -> usize {
        self.layout.size()
    }
}
impl Clone for Allocation {
    fn clone(&self) -> Self {
        self.strong
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < isize::MAX as usize).then_some(n + 1)
            })
            .expect("Shared owner count overflow");
        Self {
            block: self.block,
            budget: self.budget.clone(),
        }
    }
}
impl Drop for Allocation {
    fn drop(&mut self) {
        if self.strong.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        std::sync::atomic::fence(Ordering::Acquire);
        let bytes = self.charged_bytes();
        let mut state = self.budget.state.lock().unwrap();
        if state.stats.retained.saturating_add(bytes as u64) <= self.budget.limits.retained as u64 {
            state.stats.retained += bytes as u64;
            state.free.push(self.block);
            self.budget.capacity_changed(&mut state);
            return;
        }
        drop(state);
        let (bytes, category) = unsafe { free(self.block) };
        let mut state = self.budget.state.lock().unwrap();
        state.stats.current -= bytes as u64;
        state.remove_live(category, bytes);
        self.budget.capacity_changed(&mut state);
    }
}
impl MemoryBudget {
    pub(super) fn release_idle_for(&self, additional: usize) -> bool {
        loop {
            let block = {
                let mut state = self.state.lock().unwrap();
                if state.stats.current.checked_add(additional as u64)
                    .is_some_and(|required| required <= self.limits.total as u64)
                {
                    return true;
                }
                let Some(block) = state.free.pop() else {
                    return false;
                };
                // Publish the finite release before removing reclaimable capacity.
                // free() only drops the plain header and deallocates its Layout.
                state.active_releases = state.active_releases.checked_add(1)
                    .expect("Release count overflow");
                state.stats.retained -= unsafe { block.as_ref().layout.size() } as u64;
                block
            };
            // Actual deallocation occurs outside all budget/cache/reclaimer locks.
            let (bytes, category) = unsafe { free(block) };
            let mut state = self.state.lock().unwrap();
            state.stats.current -= bytes as u64;
            state.remove_live(category, bytes);
            state.detailed.flow.reclaimed_bytes += bytes as u64;
            state.active_releases -= 1;
            self.capacity_changed(&mut state);
        }
    }

}


impl crate::storage::BudgetRef {
    pub(super) fn allocate(
        &self,
        size: usize,
        category: ResourceCategory,
    ) -> std::io::Result<Allocation> {
        self.allocate_fixed(size,category).map_err(StorageFailure::into_io)
    }
    pub(super) fn allocate_fixed(&self,size:usize,category:ResourceCategory)->Result<Allocation,StorageFailure> {
        if size > self.limits.block {
            return Err(StorageFailure {details:StorageLimit {
                resource: "StorageBlock",
                limit: self.limits.block,
                domain_limit: self.limits.total,
                requested: size,
                current: self.statistics().current as usize,
                phase: "storage allocation",
            },kind:StorageFailureKind::PermanentLimit,terminal:false});
        }
        let size = size
            .max(4096)
            .checked_next_power_of_two()
            .unwrap_or(self.limits.block)
            .min(self.limits.block);
        {
            let mut state = self.state.lock().unwrap();
            if let Some(mut block) = state.free.take(size) {
                unsafe {
                    let header = block.as_mut();
                    let bytes = header.layout.size();
                    state.stats.retained -= bytes as u64;
                    state.remove_live(header.category, bytes);
                    state.add_live(category, bytes, false);
                    header.category = category;
                    self.allocation_reclassified(block.as_ptr().cast(), bytes, category);
                    header.strong.store(1, Ordering::Relaxed);
                }
                return Ok(Allocation {
                    block,
                    budget: self.clone(),
                });
            }
        }
        let (layout, offset) = Layout::new::<Header>()
            .extend(
                Layout::array::<u8>(size)
                    .map_err(|_| StorageFailure::at(self,category,size,"storage layout",StorageFailureKind::Overflow))?,
            )
            .map_err(|_| StorageFailure::at(self,category,size,"storage layout",StorageFailureKind::Overflow))?;
        let layout = layout.pad_to_align();
        let mut charge = self.try_reserve(layout.size(), category)?;
        let failure=||StorageFailure::at(self,category,layout.size(),"storage allocation",StorageFailureKind::SystemAllocation);
        self.allocation_attempt().map_err(|_|failure())?;
        let block = NonNull::new(unsafe { alloc_zeroed(layout) }.cast::<Header>())
            .ok_or_else(failure)?;
        unsafe {
            block.as_ptr().write(Header {
                strong: AtomicUsize::new(1),
                references: ReferenceCounts::default(),
                category,
                pointer: block.as_ptr().cast::<u8>().add(offset),
                length: size,
                layout,
                next: None,
            });
        }
        charge.commit(layout.size());
        self.allocation_committed(block.as_ptr().cast(), layout.size(), category);
        // Transfer the already committed charge to the block. Idle ownership does
        // not hold an Arc back to the domain; Allocation/IdlePool release the charge.
        charge.bytes = 0;
        charge.live = 0;
        Ok(Allocation {
            block,
            budget: self.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_storage_failures_preserve_limits_and_rollback() {
        let domain=BudgetRef::new(BudgetLimits {total:BudgetRef::allocation_size()+16384,block:4096,retained:0}).unwrap();
        let failure=domain.allocate_fixed(4097,ResourceCategory::Input).err().unwrap();
        assert_eq!(failure.kind,StorageFailureKind::PermanentLimit);
        assert_eq!(failure.details.resource,"StorageBlock");
        assert_eq!(failure.details.requested,4097);
        let occupied=domain.try_reserve(16384,ResourceCategory::Scratch).unwrap();
        let failure=domain.allocate_fixed(4096,ResourceCategory::Input).err().unwrap();
        assert_eq!(failure.kind,StorageFailureKind::BudgetUnavailable);
        assert_eq!(failure.details.phase,"reservation");
        drop(occupied);
        assert_eq!(domain.workload_statistics().current,0);
        let domain=BudgetRef::new(Default::default()).unwrap();
        domain.fail_allocation_at(0);
        let failure=domain.allocate_fixed(4096,ResourceCategory::Decompressed).err().unwrap();
        assert_eq!(failure.kind,StorageFailureKind::SystemAllocation);
        assert_eq!(failure.details.phase,"storage allocation");
        assert_eq!(domain.workload_statistics().current,0);
    }

    #[test]
    fn layout_and_idle_reuse_are_exact_and_no_domain_cycle_remains() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let weak = crate::storage::BudgetRef::downgrade(&domain);
        let block = domain.allocate(4096, ResourceCategory::Input).unwrap();
        let size = block.charged_bytes();
        assert!(size > 4096);
        assert_eq!(domain.workload_statistics().current, size as u64);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 1);
        let pointer = block.pointer;
        drop(block);
        assert_eq!(domain.workload_statistics().retained, size as u64);
        let block = domain
            .allocate(128, ResourceCategory::Decompressed)
            .unwrap();
        assert_eq!(block.pointer, pointer);
        let stats = domain.workload_detailed_statistics();
        assert_eq!(stats.allocation_count, 1);
        assert_eq!(stats.resources[ResourceCategory::Input as usize].live, 0);
        assert_eq!(
            stats.resources[ResourceCategory::Decompressed as usize].live,
            size as u64
        );
        drop(block);
        drop(domain);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn refused_layout_and_allocator_attempt_roll_back() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (4096) + crate::storage::BudgetRef::allocation_size(),
            block: 4096,
            retained: 0,
        }).unwrap();
        assert!(domain.allocate(4096, ResourceCategory::Input).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.allocation_attempt_count(), 1, "only the domain bootstrap allocated");
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        domain.fail_allocation_at(0);
        assert!(domain.allocate(4096, ResourceCategory::Input).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 0);
    }
    #[test]
    fn pressure_releases_idle_before_reserving_and_live_aliases_pin_storage() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (10000) + crate::storage::BudgetRef::allocation_size(),
            block: 8192,
            retained: 10000,
        }).unwrap();
        let block = domain.allocate(4096, ResourceCategory::Input).unwrap();
        let alias = block.clone();
        let size = block.charged_bytes();
        drop(block);
        assert_eq!(domain.workload_statistics().retained, 0);
        assert!(domain.reserve(10000).is_err());
        assert_eq!(domain.workload_statistics().current, size as u64);
        drop(alias);
        let reservation = domain.reserve(10000).unwrap();
        assert_eq!(domain.workload_statistics().retained, 0);
        assert_eq!(domain.workload_statistics().current, 10000);
        drop(reservation);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

#[cfg(test)]
mod reclaimed_tests {
    use super::*;
    #[test]
    fn idle_reclamation_counts_physical_capacity_once() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (1 << 20) + crate::storage::BudgetRef::allocation_size(),
            block: 1 << 20,
            retained: 1 << 20,
        }).unwrap();
        let block = domain.allocate(4096, ResourceCategory::Input).unwrap();
        let alias = block.clone();
        let charged = domain.workload_statistics().current;
        drop(block);
        assert_eq!(domain.workload_detailed_statistics().flow.reclaimed_bytes, 0);
        assert!(!domain.release_idle_for(1 << 20));
        assert_eq!(domain.workload_statistics().current, charged);
        drop(alias);
        assert_eq!(domain.workload_statistics().retained, charged);
        assert_eq!(domain.workload_detailed_statistics().flow.reclaimed_bytes, 0);
        assert!(domain.release_idle_for(1 << 20));
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.workload_detailed_statistics().flow.reclaimed_bytes, charged);
        assert!(domain.release_idle_for(1 << 20));
        assert_eq!(domain.workload_detailed_statistics().flow.reclaimed_bytes, charged);
    }
}
