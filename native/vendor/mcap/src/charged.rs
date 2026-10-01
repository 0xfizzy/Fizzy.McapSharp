//! Explicit, fallible allocation with a reservation that outlives the storage.
use crate::storage::{Reservation, ResourceCategory};
use std::{
    alloc::{alloc, dealloc, Layout},
    io,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
struct Entry<T> {
    value: ManuallyDrop<T>,
    charge: ManuallyDrop<Reservation>,
}
pub struct ChargedBox<T> {
    pointer: NonNull<Entry<T>>,
}
unsafe impl<T: Send> Send for ChargedBox<T> {}
unsafe impl<T: Sync> Sync for ChargedBox<T> {}
impl<T> ChargedBox<T> {
    /// Transfers ownership while retaining the private ABI's pointer-to-value shape.
    /// The control allocation remains private and uses only this module's own layout.
    pub fn into_raw_value(self) -> *mut T {
        let value = unsafe { std::ptr::addr_of_mut!((*self.pointer.as_ptr()).value).cast::<T>() };
        std::mem::forget(self);
        value
    }
    /// # Safety
    /// `value` must be the live pointer returned by into_raw_value for this T.
    /// The caller transfers its sole allocation ownership and must stop using it.
    pub unsafe fn from_raw_value(value: *mut T) -> Self {
        let allocation = value
            .cast::<u8>()
            .sub(std::mem::offset_of!(Entry<T>, value))
            .cast::<Entry<T>>();
        Self {
            pointer: NonNull::new_unchecked(allocation),
        }
    }
    /// Transfers this allocation into an opaque native handle without allocating.
    pub fn into_handle(self) -> NonNull<()> {
        let pointer = self.pointer.cast();
        std::mem::forget(self);
        pointer
    }
    /// # Safety
    /// The handle must come from into_handle for this T, remain live, and have
    /// exactly one owner transferring it back. It must not be used after this call.
    pub unsafe fn from_handle(pointer: NonNull<()>) -> Self {
        Self {
            pointer: pointer.cast(),
        }
    }
    /// # Safety
    /// The handle must come from into_handle for this T and remain live and
    /// immutable throughout the returned borrow. The caller chooses that lifetime.
    pub unsafe fn borrow_handle<'a>(pointer: NonNull<()>) -> &'a T {
        &*pointer.cast::<Entry<T>>().as_ref().value
    }
    pub fn allocation_size() -> usize {
        Layout::new::<Entry<T>>().size()
    }
    pub fn allocation_bytes(&self) -> usize {
        Self::allocation_size()
    }
    pub fn charge_owner(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        unsafe {
            self.pointer.as_ref().charge.owner_reference(kind, acquire);
        }
    }

    pub fn new(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        Self::new_fixed(value, domain, category).map_err(crate::storage::StorageFailure::into_io)
    }
    pub fn new_fixed(value: T, domain: &crate::storage::BudgetRef, category: ResourceCategory)
        -> Result<Self, crate::storage::StorageFailure> {
        let layout = Layout::new::<Entry<T>>();
        let mut charge = domain.reserve_class_fixed(layout.size(), category)
            .map_err(crate::storage::StorageFailure::from)?;
        let failure = || crate::storage::StorageFailure::at(domain, category, layout.size(),
            "control allocation", crate::storage::StorageFailureKind::SystemAllocation);
        domain.allocation_attempt().map_err(|_| failure())?;
        let pointer = NonNull::new(unsafe { alloc(layout) }.cast::<Entry<T>>()).ok_or_else(failure)?;
        charge.commit(layout.size());
        domain.allocation_committed(pointer.as_ptr().cast(), layout.size(), category);
        unsafe {
            pointer.as_ptr().write(Entry {
                value: ManuallyDrop::new(value),
                charge: ManuallyDrop::new(charge),
            });
        }
        Ok(Self { pointer })
    }
}
impl<T> Deref for ChargedBox<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &self.pointer.as_ref().value }
    }
}
impl<T> DerefMut for ChargedBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut self.pointer.as_mut().value }
    }
}
struct Release {
    pointer: NonNull<u8>,
    layout: Layout,
    charge: Option<Reservation>,
    // Dropped after the value, physical allocation, and ledger charge, including
    // during unwinding from a value destructor. This is a finite native scope.
    _capacity_release: crate::storage::CapacityReleaseGuard,
}
impl Drop for Release {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.pointer.as_ptr(), self.layout);
        }
        drop(self.charge.take());
    }
}
impl<T> Drop for ChargedBox<T> {
    fn drop(&mut self) {
        unsafe {
            let entry = self.pointer.as_mut();
            let capacity_release = entry.charge.domain().begin_capacity_release();
            let release = Release {
                pointer: self.pointer.cast(),
                layout: Layout::new::<Entry<T>>(),
                charge: Some(ManuallyDrop::take(&mut entry.charge)),
                _capacity_release: capacity_release,
            };
            ManuallyDrop::drop(&mut entry.value);
            drop(release);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_handle_roundtrip_preserves_alignment_and_charge() {
        #[repr(align(128))]
        struct Aligned(u64);
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = ChargedBox::new(Aligned(42), &domain, ResourceCategory::Descriptor).unwrap();
        let bytes = value.allocation_bytes() as u64;
        let handle = value.into_handle();
        assert_eq!(handle.as_ptr() as usize % 128, 0);
        assert_eq!(
            unsafe { ChargedBox::<Aligned>::borrow_handle(handle) }.0,
            42
        );
        assert_eq!(domain.workload_statistics().current, bytes);
        drop(unsafe { ChargedBox::<Aligned>::from_handle(handle) });
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn value_pointer_roundtrip_supports_alignment_mutation_and_zero_sized_values() {
        #[repr(align(128))]
        struct Aligned([u8; 17]);
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = ChargedBox::new(Aligned([7; 17]), &domain, ResourceCategory::Scratch).unwrap();
        let capacity = value.allocation_bytes() as u64;
        let pointer = value.into_raw_value();
        assert_eq!(pointer as usize % 128, 0);
        unsafe {
            (*pointer).0[2] = 42;
        }
        assert_eq!(domain.workload_statistics().current, capacity);
        let value = unsafe { ChargedBox::<Aligned>::from_raw_value(pointer) };
        assert_eq!(value.0[2], 42);
        drop(value);
        let zero = ChargedBox::new((), &domain, ResourceCategory::Scratch).unwrap();
        let pointer = zero.into_raw_value();
        drop(unsafe { ChargedBox::<()>::from_raw_value(pointer) });
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn charges_actual_layout_and_releases() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = ChargedBox::new(42u64, &domain, ResourceCategory::Scratch).unwrap();
        assert_eq!(*value, 42);
        assert_eq!(
            domain.workload_statistics().current,
            Layout::new::<Entry<u64>>().size() as u64
        );
        drop(value);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn refusal_does_not_allocate_or_publish() {
        let domain = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: (1) + crate::storage::BudgetRef::allocation_size(),
            block: 1,
            retained: 0,
        }).unwrap();
        assert!(ChargedBox::new(42u64, &domain, ResourceCategory::Scratch).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 0);
    }
}

/// Allocates exactly the requested byte capacity; the owner must drop data before its charge.
pub fn bytes(
    domain: &crate::storage::BudgetRef,
    category: ResourceCategory,
    capacity: usize,
) -> io::Result<(Vec<u8>, Reservation)> {
    vector(domain, category, capacity)
}

/// An exact-capacity vector. Callers must never grow it without another transaction.
pub fn vector<T>(
    domain: &crate::storage::BudgetRef,
    category: ResourceCategory,
    capacity: usize,
) -> io::Result<(Vec<T>, Reservation)> {
    vector_fixed(domain, category, capacity).map_err(|failure| {
        if failure.kind == crate::storage::StorageFailureKind::Overflow {
            io::ErrorKind::InvalidInput.into()
        } else { failure.into_io() }
    })
}
pub fn vector_fixed<T>(domain: &crate::storage::BudgetRef, category: ResourceCategory, capacity: usize)
    -> Result<(Vec<T>, Reservation), crate::storage::StorageFailure> {
    use crate::storage::{StorageFailure, StorageFailureKind};
    let layout = Layout::array::<T>(capacity).map_err(|_| StorageFailure::at(domain, category,
        capacity.saturating_mul(std::mem::size_of::<T>()), "vector layout", StorageFailureKind::Overflow))?;
    let mut charge = domain.reserve_class_fixed(layout.size(), category).map_err(StorageFailure::from)?;
    if layout.size() == 0 { return Ok((Vec::new(), charge)); }
    let failure = || StorageFailure::at(domain, category, layout.size(), "vector allocation", StorageFailureKind::SystemAllocation);
    domain.allocation_attempt().map_err(|_| failure())?;
    let raw = unsafe { alloc(layout) }.cast::<T>();
    if raw.is_null() { return Err(failure()); }
    charge.commit(layout.size());
    domain.allocation_committed(raw.cast(), layout.size(), category);
    Ok((unsafe { Vec::from_raw_parts(raw, 0, capacity) }, charge))
}

struct SharedValue<T> {
    owners: std::sync::atomic::AtomicUsize,
    value: T,
}
/// Shared ownership with an explicit, charged control-block layout and no weak references.
pub struct ChargedShared<T> {
    pointer: NonNull<Entry<SharedValue<T>>>,
}
unsafe impl<T: Send + Sync> Send for ChargedShared<T> {}
unsafe impl<T: Send + Sync> Sync for ChargedShared<T> {}
impl<T> ChargedShared<T> {
    /// Covers removal of explicit owner pins through the corresponding release.
    pub fn begin_capacity_release(&self) -> crate::storage::CapacityReleaseGuard {
        unsafe { self.pointer.as_ref().charge.domain().begin_capacity_release() }
    }
    pub fn allocation_bytes(&self) -> usize {
        Layout::new::<Entry<SharedValue<T>>>().size()
    }
    pub fn charge_owner(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        unsafe {
            self.pointer.as_ref().charge.owner_reference(kind, acquire);
        }
    }
    pub fn new(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        Self::new_fixed(value,domain,category).map_err(crate::storage::StorageFailure::into_io)
    }
    pub fn new_fixed(value:T, domain:&crate::storage::BudgetRef, category:ResourceCategory)
        -> Result<Self,crate::storage::StorageFailure> {
        let allocation = ChargedBox::new_fixed(
            SharedValue {
                owners: std::sync::atomic::AtomicUsize::new(1),
                value,
            },
            domain,
            category,
        )?;
        let pointer = allocation.pointer;
        std::mem::forget(allocation);
        Ok(Self { pointer })
    }
    pub fn get_mut(&mut self) -> Option<&mut T> {
        let unique = unsafe {
            self.pointer
                .as_ref()
                .value
                .owners
                .load(std::sync::atomic::Ordering::Acquire)
                == 1
        };
        if unique {
            Some(unsafe { &mut self.pointer.as_mut().value.value })
        } else {
            None
        }
    }
}
impl<T> Deref for ChargedShared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &self.pointer.as_ref().value.value }
    }
}
impl<T> Clone for ChargedShared<T> {
    fn clone(&self) -> Self {
        let owners = unsafe { &self.pointer.as_ref().value.owners };
        owners
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| {
                    if n < isize::MAX as usize {
                        Some(n + 1)
                    } else {
                        None
                    }
                },
            )
            .expect("Shared owner count overflow");
        Self {
            pointer: self.pointer,
        }
    }
}
impl<T> Drop for ChargedShared<T> {
    fn drop(&mut self) {
        let owners = unsafe { &self.pointer.as_ref().value.owners };
        if owners.fetch_sub(1, std::sync::atomic::Ordering::Release) == 1 {
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            drop(ChargedBox {
                pointer: self.pointer,
            });
        }
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    #[test]
    fn tree_owner_removal_is_inside_physical_release_scope() {
        use crate::storage::{BudgetLimits, CapacityWaitStatus, OwnerKind};
        struct Tree(Reservation);
        impl TreeOwnership for Tree {
            fn reference_children(&self, owner: OwnerKind, acquire: bool) {
                self.0.owner_reference(owner, acquire);
                if !acquire {
                    assert_eq!(self.0.domain().retry_status(4096).unwrap(), CapacityWaitStatus::Waiting);
                }
            }
            fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
        }
        let domain = crate::storage::BudgetRef::new(BudgetLimits {total: (65536) + crate::storage::BudgetRef::allocation_size(), block: 65536, retained: 0}).unwrap();
        let tree = ChargedTree::new_owned(Tree(domain.reserve(4096).unwrap()), &domain,
            ResourceCategory::Index, OwnerKind::Lease).unwrap();
        let fixed = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
        drop(tree);
        assert_eq!(domain.retry_status(4096).unwrap(), CapacityWaitStatus::Ready);
        drop(fixed);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn charged_destructor_keeps_admission_open_through_unwind_and_cleanup() {
        use crate::storage::{BudgetLimits, CapacityWaitStatus, OwnerKind};
        struct Value { charge: Reservation, panic: bool }
        impl Drop for Value {
            fn drop(&mut self) {
                self.charge.owner_reference(OwnerKind::Lease, false);
                assert_eq!(self.charge.domain().retry_status(4096).unwrap(), CapacityWaitStatus::Waiting);
                assert!(!self.panic, "injected destructor failure");
            }
        }
        for panic in [false, true] {
            let domain = crate::storage::BudgetRef::new(BudgetLimits { total: (65536) + crate::storage::BudgetRef::allocation_size(), block: 65536, retained: 0 }).unwrap();
            let charge = domain.reserve(4096).unwrap();
            charge.owner_reference(OwnerKind::Lease, true);
            let value = ChargedShared::new(Value { charge, panic }, &domain, ResourceCategory::Scratch).unwrap();
            let fixed = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
            assert_eq!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).is_err(), panic);
            assert_eq!(domain.retry_status(4096).unwrap(), CapacityWaitStatus::Ready);
            let refill = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
            // A leaked release scope would incorrectly keep this request waiting.
            assert!(domain.retry_status(4096).is_err());
            drop((refill, fixed));
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
    #[test]
    fn concurrent_owners_retain_one_charge() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut root = ChargedShared::new(42usize, &domain, ResourceCategory::Index).unwrap();
        let bytes = domain.workload_statistics().current;
        let other = root.clone();
        assert!(root.get_mut().is_none());
        std::thread::spawn(move || {
            for _ in 0..1000 {
                assert_eq!(*other.clone(), 42);
            }
        })
        .join()
        .unwrap();
        assert_eq!(domain.workload_statistics().current, bytes);
        *root.get_mut().unwrap() = 43;
        assert_eq!(*root, 43);
        drop(root);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn destructor_panic_still_returns_storage_charge() {
        struct PanicDrop;
        impl Drop for PanicDrop {
            fn drop(&mut self) {
                panic!("injected destructor failure");
            }
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let value = ChargedBox::new(PanicDrop, &domain, ResourceCategory::Scratch).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

/// Child allocation ownership for an immutable shared tree.
pub trait TreeOwnership {
    fn reference_children(&self, kind: crate::storage::OwnerKind, acquire: bool);
    /// While uniquely borrowed, new child allocations inherit this pin before
    /// publication. Existing pins remain intact, including during unwinding.
    fn mutation_owner(&mut self, owner: Option<crate::storage::OwnerKind>);
}
/// Keeps control and child allocations pinned for the same owner lifetime.
pub struct ChargedTree<T: TreeOwnership> {
    root: ManuallyDrop<ChargedShared<T>>,
    owner: crate::storage::OwnerKind,
}
impl<T: TreeOwnership> ChargedTree<T> {
    pub fn new(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        Self::new_owned(value, domain, category, crate::storage::OwnerKind::Parser)
    }
    pub(crate) fn new_owned(
        value: T,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: crate::storage::OwnerKind,
    ) -> io::Result<Self> {
        Self::new_owned_fixed(value,domain,category,owner).map_err(crate::storage::StorageFailure::into_io)
    }
    pub(crate) fn new_owned_fixed(value:T, domain:&crate::storage::BudgetRef,
        category:ResourceCategory, owner:crate::storage::OwnerKind)
        -> Result<Self,crate::storage::StorageFailure> {
        let result = Self {
            root: ManuallyDrop::new(ChargedShared::new_fixed(value, domain, category)?),
            owner,
        };
        result.reference(true);
        Ok(result)
    }
    fn reference(&self, acquire: bool) {
        self.root.charge_owner(self.owner, acquire);
        self.root.reference_children(self.owner, acquire);
    }
    pub fn get_mut(&mut self) -> Option<TreeMutation<'_, T>> {
        self.root.get_mut()?.mutation_owner(Some(self.owner));
        Some(TreeMutation(self))
    }
    pub fn clone_with_owner(&self, owner: crate::storage::OwnerKind) -> Self {
        let result = Self {
            root: self.root.clone(),
            owner,
        };
        result.reference(true);
        result
    }
}
impl<T: TreeOwnership> Clone for ChargedTree<T> {
    fn clone(&self) -> Self {
        self.clone_with_owner(self.owner)
    }
}
impl<T: TreeOwnership> Deref for ChargedTree<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.root
    }
}
impl<T: TreeOwnership> Drop for ChargedTree<T> {
    fn drop(&mut self) {
        let _release = self.root.begin_capacity_release();
        // Move into a local owner so unwinding also destroys the root before
        // ending the release scope. The field must not be dropped a second time.
        let root = unsafe { ManuallyDrop::take(&mut self.root) };
        root.charge_owner(self.owner, false);
        root.reference_children(self.owner, false);
        drop(root);
    }
}
/// Keeps existing allocations pinned; only newly allocated children acquire a
/// pin during mutation. Clearing the mutation policy never traverses the tree.
pub struct TreeMutation<'a, T: TreeOwnership>(&'a mut ChargedTree<T>);
impl<T: TreeOwnership> Deref for TreeMutation<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.root
    }
}
impl<T: TreeOwnership> DerefMut for TreeMutation<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.root.get_mut().unwrap()
    }
}
impl<T: TreeOwnership> Drop for TreeMutation<'_, T> {
    fn drop(&mut self) {
        self.0.root.get_mut().unwrap().mutation_owner(None);
    }
}

#[path = "weak_shared.rs"]
pub mod weak;

#[path = "charged_source.rs"]
mod source;
pub use source::ChargedSource;
