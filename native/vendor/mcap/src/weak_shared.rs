//! Explicit shared control layout with weak references and no domain ownership cycle.
use crate::storage::{DetachedReservation, OwnerKind, ResourceCategory};
use std::{
    alloc::{alloc, dealloc, Layout},
    cell::UnsafeCell,
    io,
    mem::ManuallyDrop,
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Entry<T> {
    strong: AtomicUsize,
    // Includes one implicit weak reference while the value is alive.
    weak: AtomicUsize,
    value: UnsafeCell<ManuallyDrop<T>>,
    charge: ManuallyDrop<DetachedReservation>,
}
pub struct BudgetedArc<T> {
    owner: OwnerKind,
    pointer: NonNull<Entry<T>>,
    domain: crate::storage::BudgetRef,
}
pub struct BudgetedWeak<T> {
    pointer: NonNull<Entry<T>>,
    domain: crate::storage::BudgetWeak,
}
unsafe impl<T: Send + Sync> Send for BudgetedArc<T> {}
unsafe impl<T: Send + Sync> Sync for BudgetedArc<T> {}
unsafe impl<T: Send + Sync> Send for BudgetedWeak<T> {}
unsafe impl<T: Send + Sync> Sync for BudgetedWeak<T> {}
fn increment(counter: &AtomicUsize) {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < isize::MAX as usize).then_some(n + 1)
        })
        .expect("Shared owner count overflow");
}
impl<T> BudgetedArc<T> {
    pub fn new(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        Self::new_with_owner(value, domain, category, OwnerKind::Operation)
    }
    pub fn new_with_owner(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: OwnerKind,
    ) -> io::Result<Self> {
        Self::new_with_owner_fixed(value, domain, category, owner)
            .map_err(crate::storage::StorageFailure::into_io)
    }
    pub fn new_fixed(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> Result<Self, crate::storage::StorageFailure> {
        Self::new_with_owner_fixed(value, domain, category, OwnerKind::Operation)
    }
    pub fn new_with_owner_fixed(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: OwnerKind,
    ) -> Result<Self, crate::storage::StorageFailure> {
        let layout = Layout::new::<Entry<T>>();
        let mut charge = domain.reserve_class_fixed(layout.size(), category)
            .map_err(crate::storage::StorageFailure::from)?;
        let failure = || crate::storage::StorageFailure::at(domain, category, layout.size(),
            "shared control allocation", crate::storage::StorageFailureKind::SystemAllocation);
        domain.allocation_attempt().map_err(|_| failure())?;
        let pointer = NonNull::new(unsafe { alloc(layout) }.cast::<Entry<T>>())
            .ok_or_else(failure)?;
        charge.commit(layout.size());
        domain.allocation_committed(pointer.as_ptr().cast(), layout.size(), category);
        charge.owner_reference(owner, true);
        unsafe {
            pointer.as_ptr().write(Entry {
                strong: AtomicUsize::new(1),
                weak: AtomicUsize::new(1),
                value: UnsafeCell::new(ManuallyDrop::new(value)),
                charge: ManuallyDrop::new(charge.detach()),
            });
        }
        Ok(Self {
            owner,
            pointer,
            domain: domain.clone(),
        })
    }
    pub fn downgrade(&self) -> BudgetedWeak<T> {
        increment(unsafe { &self.pointer.as_ref().weak });
        BudgetedWeak {
            pointer: self.pointer,
            domain: crate::storage::BudgetRef::downgrade(&self.domain),
        }
    }
    pub fn allocation_size() -> usize {
        Layout::new::<Entry<T>>().size()
    }
    pub fn allocation_bytes(&self) -> usize {
        Self::allocation_size()
    }
    /// Shares the same charged control under a different retention purpose.
    /// This updates this control's pin, not the value's independent child owners.
    pub fn clone_with_owner(&self, owner: OwnerKind) -> Self {
        increment(unsafe { &self.pointer.as_ref().strong });
        unsafe { self.pointer.as_ref().charge.owner_reference(owner, true); }
        Self { owner, pointer: self.pointer, domain: self.domain.clone() }
    }
}
impl<T> Clone for BudgetedArc<T> {
    fn clone(&self) -> Self {
        self.clone_with_owner(self.owner)
    }
}
impl<T> Deref for BudgetedArc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.pointer.as_ref().value.get() }
    }
}
struct ReleaseWeak<T>(NonNull<Entry<T>>);
impl<T> Drop for ReleaseWeak<T> {
    fn drop(&mut self) {
        unsafe {
            if self.0.as_ref().weak.fetch_sub(1, Ordering::Release) != 1 {
                return;
            }
            std::sync::atomic::fence(Ordering::Acquire);
            let charge = ManuallyDrop::take(&mut self.0.as_mut().charge);
            dealloc(self.0.as_ptr().cast(), Layout::new::<Entry<T>>());
            drop(charge);
        }
    }
}
impl<T> Drop for BudgetedArc<T> {
    fn drop(&mut self) {
        // Start before removing the owner marker; a concurrent waiter must not
        // mistake the last owner's in-progress destruction for permanent usage.
        // Weak references may retain the control: scope exit rechecks capacity.
        let _release = self.domain.begin_capacity_release();
        unsafe {
            self.pointer
                .as_ref()
                .charge
                .owner_reference(self.owner, false);
            if self.pointer.as_ref().strong.fetch_sub(1, Ordering::Release) != 1 {
                return;
            }
            std::sync::atomic::fence(Ordering::Acquire);
            let release = ReleaseWeak(self.pointer);
            ManuallyDrop::drop(&mut *self.pointer.as_ref().value.get());
            drop(release);
        }
    }
}
impl<T> BudgetedWeak<T> {
    pub fn upgrade(&self) -> Option<BudgetedArc<T>> {
        let domain = self.domain.upgrade()?;
        let strong = unsafe { &self.pointer.as_ref().strong };
        strong
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |n| {
                if n == 0 {
                    None
                } else {
                    assert!(n < isize::MAX as usize, "Shared owner count overflow");
                    Some(n + 1)
                }
            })
            .ok()?;
        unsafe {
            self.pointer
                .as_ref()
                .charge
                .owner_reference(OwnerKind::Operation, true);
        }
        Some(BudgetedArc {
            owner: OwnerKind::Operation,
            pointer: self.pointer,
            domain,
        })
    }
    pub fn strong_count(&self) -> usize {
        unsafe { self.pointer.as_ref().strong.load(Ordering::Acquire) }
    }
}
impl<T> Clone for BudgetedWeak<T> {
    fn clone(&self) -> Self {
        increment(unsafe { &self.pointer.as_ref().weak });
        Self {
            pointer: self.pointer,
            domain: self.domain.clone(),
        }
    }
}
impl<T> Drop for BudgetedWeak<T> {
    fn drop(&mut self) {
        drop(ReleaseWeak(self.pointer));
    }
}

// Only this module constructs erased handles. The vtable always matches Entry<T>;
// no trait-object layout or allocating adapter is involved.
struct ReclaimerVtable {
    clone_weak: unsafe fn(NonNull<()>),
    drop_weak: unsafe fn(NonNull<()>),
    strong_count: unsafe fn(NonNull<()>) -> usize,
    upgrade: unsafe fn(NonNull<()>) -> bool,
    drop_strong: unsafe fn(NonNull<()>, crate::storage::BudgetRef),
    last_access: unsafe fn(NonNull<()>) -> u64,
    reclaim: unsafe fn(NonNull<()>) -> bool,
}
impl<T: crate::storage::Reclaimable + 'static> BudgetedArc<T> {
    const RECLAIMER: ReclaimerVtable = ReclaimerVtable {
        clone_weak: |p| unsafe { increment(&p.cast::<Entry<T>>().as_ref().weak) },
        drop_weak: |p| drop(ReleaseWeak(p.cast::<Entry<T>>())),
        strong_count: |p| unsafe { p.cast::<Entry<T>>().as_ref().strong.load(Ordering::Acquire) },
        upgrade: |p| unsafe {
            let upgraded = p
                .cast::<Entry<T>>()
                .as_ref()
                .strong
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, |n| {
                    if n == 0 {
                        None
                    } else {
                        assert!(n < isize::MAX as usize, "Shared owner count overflow");
                        Some(n + 1)
                    }
                })
                .is_ok();
            if upgraded {
                p.cast::<Entry<T>>()
                    .as_ref()
                    .charge
                    .owner_reference(OwnerKind::Operation, true);
            }
            upgraded
        },
        drop_strong: |p, domain| {
            drop(BudgetedArc::<T> {
                owner: OwnerKind::Operation,
                pointer: p.cast(),
                domain,
            })
        },
        last_access: |p| unsafe { (&*p.cast::<Entry<T>>().as_ref().value.get()).last_access() },
        reclaim: |p| unsafe { (&*p.cast::<Entry<T>>().as_ref().value.get()).reclaim() },
    };
    pub fn reclaimer(&self) -> WeakReclaimer {
        increment(unsafe { &self.pointer.as_ref().weak });
        WeakReclaimer {
            pointer: self.pointer.cast(),
            domain: crate::storage::BudgetRef::downgrade(&self.domain),
            vtable: &Self::RECLAIMER,
        }
    }
}
pub struct WeakReclaimer {
    pointer: NonNull<()>,
    domain: crate::storage::BudgetWeak,
    vtable: &'static ReclaimerVtable,
}
pub struct ReclaimerPin {
    pointer: NonNull<()>,
    domain: crate::storage::BudgetRef,
    vtable: &'static ReclaimerVtable,
}
// Construction requires T: Reclaimable (Send + Sync). A weak pins the control;
// a strong pins both its typed value and the budget domain.
unsafe impl Send for WeakReclaimer {}
unsafe impl Sync for WeakReclaimer {}
unsafe impl Send for ReclaimerPin {}
unsafe impl Sync for ReclaimerPin {}
impl WeakReclaimer {
    pub fn strong_count(&self) -> usize {
        unsafe { (self.vtable.strong_count)(self.pointer) }
    }
    pub fn upgrade(&self) -> Option<ReclaimerPin> {
        let domain = self.domain.upgrade()?;
        if !unsafe { (self.vtable.upgrade)(self.pointer) } {
            return None;
        }
        Some(ReclaimerPin {
            pointer: self.pointer,
            domain,
            vtable: self.vtable,
        })
    }
}
impl Clone for WeakReclaimer {
    fn clone(&self) -> Self {
        unsafe {
            (self.vtable.clone_weak)(self.pointer);
        }
        Self {
            pointer: self.pointer,
            domain: self.domain.clone(),
            vtable: self.vtable,
        }
    }
}
impl Drop for WeakReclaimer {
    fn drop(&mut self) {
        unsafe {
            (self.vtable.drop_weak)(self.pointer);
        }
    }
}
impl crate::storage::Reclaimable for ReclaimerPin {
    fn last_access(&self) -> u64 {
        unsafe { (self.vtable.last_access)(self.pointer) }
    }
    fn reclaim(&self) -> bool {
        unsafe { (self.vtable.reclaim)(self.pointer) }
    }
}
impl Drop for ReclaimerPin {
    fn drop(&mut self) {
        unsafe {
            (self.vtable.drop_strong)(self.pointer, self.domain.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clones_can_change_control_owner_without_new_storage() {
        use crate::storage::{BudgetRef, OwnerKind, ResourceCategory};
        let domain = BudgetRef::default();
        let operation = super::BudgetedArc::new(42u64, &domain, ResourceCategory::Declaration).unwrap();
        let size = operation.allocation_bytes() as u64;
        let allocations = domain.detailed_statistics().allocation_count;
        let parser = operation.clone_with_owner(OwnerKind::Parser);
        let another_parser = parser.clone();
        assert_eq!(domain.detailed_statistics().allocation_count, allocations);
        let owners = domain.ownership_statistics();
        assert_eq!(owners.bytes[OwnerKind::Operation as usize], size);
        assert_eq!(owners.bytes[OwnerKind::Parser as usize], size);
        drop(operation);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Operation as usize], 0);
        drop(parser);
        assert_eq!(*another_parser, 42);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize], size);
        drop(another_parser);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    use super::*;
    #[test]
    fn last_strong_release_does_not_treat_surviving_weak_control_as_freed() {
        use crate::storage::{BudgetLimits, CapacityWaitStatus, CapacityWaitTicket};
        struct Value(crate::storage::BudgetRef);
        impl Drop for Value {
            fn drop(&mut self) {
                assert_eq!(self.0.retry_status(1).unwrap(), CapacityWaitStatus::Waiting);
            }
        }
        let domain = crate::storage::BudgetRef::new(BudgetLimits {total: (65536) + crate::storage::BudgetRef::allocation_size(), block: 65536, retained: 0}).unwrap();
        let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
        let value = BudgetedArc::new_with_owner(Value(domain.clone()), &domain,
            ResourceCategory::Scratch, OwnerKind::Lease).unwrap();
        let weak = value.downgrade();
        let fixed = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
        assert_eq!(ticket.arm_for_external_release(1).unwrap(), CapacityWaitStatus::Waiting);
        drop(value);
        assert!(weak.upgrade().is_none());
        assert_eq!(ticket.status(), CapacityWaitStatus::Unavailable);
        assert!(ticket.checked_status().is_err());
        // Only the last weak reference really returns the control allocation.
        drop(weak);
        assert_eq!(ticket.checked_status().unwrap(), CapacityWaitStatus::Ready);
        drop((ticket, fixed));
        assert_eq!(domain.workload_statistics().current, 0);
    }
    struct Candidate(AtomicUsize);
    impl crate::storage::Reclaimable for Candidate {
        fn last_access(&self) -> u64 {
            17
        }
        fn reclaim(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) == 0
        }
    }
    #[test]
    fn shared_control_ownership_is_unique_and_upgrade_is_an_operation() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = BudgetedArc::new_with_owner(
            Candidate(AtomicUsize::new(0)),
            &domain,
            ResourceCategory::Scratch,
            OwnerKind::Parser,
        )
        .unwrap();
        let bytes = value.allocation_bytes() as u64;
        let alias = value.clone();
        let weak = value.downgrade();
        let erased = value.reclaimer();
        let typed_pin = weak.upgrade().unwrap();
        let erased_pin = erased.upgrade().unwrap();
        let owners = domain.ownership_statistics();
        assert_eq!(owners.bytes, [bytes, 0, bytes, 0, 0]);
        assert_eq!(owners.immediately_reclaimable, 0);
        drop(value);
        assert_eq!(domain.ownership_statistics().bytes[0], bytes);
        drop(alias);
        assert_eq!(domain.ownership_statistics().bytes, [0, 0, bytes, 0, 0]);
        drop(typed_pin);
        assert_eq!(domain.ownership_statistics().bytes[2], bytes);
        drop(erased_pin);
        assert_eq!(domain.ownership_statistics(), Default::default());
        assert_eq!(domain.workload_statistics().current, bytes);
        drop(weak);
        drop(erased);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn erased_reclaimer_preserves_value_and_control_lifetimes() {
        use crate::storage::Reclaimable;
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let item = BudgetedArc::new(
            Candidate(AtomicUsize::new(0)),
            &domain,
            ResourceCategory::Scratch,
        )
        .unwrap();
        let capacity = item.allocation_bytes() as u64;
        let weak = item.reclaimer();
        let alias = weak.clone();
        let pin = weak.upgrade().unwrap();
        drop(item);
        assert_eq!(alias.strong_count(), 1);
        assert_eq!(pin.last_access(), 17);
        assert!(pin.reclaim());
        assert!(!pin.reclaim());
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 1);
        drop(pin);
        assert!(alias.upgrade().is_none());
        drop(weak);
        assert_eq!(domain.workload_statistics().current, capacity);
        drop(alias);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn erased_weak_does_not_retain_domain() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let observer = crate::storage::BudgetRef::downgrade(&domain);
        let item = BudgetedArc::new(
            Candidate(AtomicUsize::new(0)),
            &domain,
            ResourceCategory::Scratch,
        )
        .unwrap();
        let weak = item.reclaimer();
        drop(item);
        drop(domain);
        assert!(observer.upgrade().is_none());
        assert!(weak.upgrade().is_none());
        drop(weak);
    }
    #[test]
    fn erased_concurrent_upgrade_cannot_resurrect_value() {
        use crate::storage::Reclaimable;
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let item = BudgetedArc::new(
            Candidate(AtomicUsize::new(0)),
            &domain,
            ResourceCategory::Scratch,
        )
        .unwrap();
        let weak = item.reclaimer();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let weak = weak.clone();
                scope.spawn(move || {
                    for _ in 0..10000 {
                        if let Some(pin) = weak.upgrade() {
                            assert_eq!(pin.last_access(), 17);
                        }
                    }
                });
            }
            drop(item);
        });
        assert!(weak.upgrade().is_none());
        drop(weak);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn value_and_control_have_distinct_release_points() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = BudgetedArc::new(42u64, &domain, ResourceCategory::Scratch).unwrap();
        let capacity = value.allocation_bytes() as u64;
        let weak = value.downgrade();
        assert_eq!(domain.workload_statistics().current, capacity);
        let alias = weak.upgrade().unwrap();
        assert_eq!(*alias, 42);
        drop(value);
        drop(alias);
        assert_eq!(weak.strong_count(), 0);
        assert!(weak.upgrade().is_none());
        assert_eq!(domain.workload_statistics().current, capacity);
        drop(weak);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn weak_control_does_not_keep_domain_alive() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let observer = crate::storage::BudgetRef::downgrade(&domain);
        let value = BudgetedArc::new(42, &domain, ResourceCategory::Scratch).unwrap();
        let weak = value.downgrade();
        drop(value);
        drop(domain);
        assert!(observer.upgrade().is_none());
        assert!(weak.upgrade().is_none());
        drop(weak);
    }
    #[test]
    fn refused_control_allocation_rolls_back() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        domain.fail_allocation_at(0);
        assert!(BudgetedArc::new(42, &domain, ResourceCategory::Scratch).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 0);
    }
    #[test]
    fn panicking_value_drop_still_releases_control() {
        struct Panics;
        impl Drop for Panics {
            fn drop(&mut self) {
                panic!("injected destructor failure");
            }
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = BudgetedArc::new(Panics, &domain, ResourceCategory::Scratch).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value)));
        assert!(result.is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn concurrent_upgrade_cannot_resurrect_dropped_value() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = BudgetedArc::new(42, &domain, ResourceCategory::Scratch).unwrap();
        let weak = value.downgrade();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let weak = weak.clone();
                scope.spawn(move || {
                    for _ in 0..10000 {
                        if let Some(v) = weak.upgrade() {
                            assert_eq!(*v, 42);
                        }
                    }
                });
            }
            drop(value);
        });
        assert!(weak.upgrade().is_none());
        drop(weak);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
