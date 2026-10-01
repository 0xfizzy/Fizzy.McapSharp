//! Type-erased ownership of an explicitly charged shared source.
//! The dispatch table describes our own allocation, never an Arc or trait-object layout.
use super::ChargedShared;
use crate::storage::{OwnerKind, SharedSource};
use std::{mem::ManuallyDrop, ptr::NonNull};
struct Vtable {
    clone: unsafe fn(NonNull<()>),
    drop: unsafe fn(NonNull<()>),
    bytes: unsafe fn(NonNull<()>) -> (*const u8, usize),
    owner: unsafe fn(NonNull<()>, OwnerKind, bool),
    reference: unsafe fn(NonNull<()>, bool, bool),
    release_scope: unsafe fn(NonNull<()>) -> crate::storage::CapacityReleaseGuard,
}
pub struct ChargedSource {
    pointer: NonNull<()>,
    vtable: &'static Vtable,
}
// The only constructor requires SharedSource: Send + Sync and a static owned value.
unsafe impl Send for ChargedSource {}
unsafe impl Sync for ChargedSource {}
impl<T: SharedSource + 'static> ChargedShared<T> {
    const SOURCE_VTABLE: Vtable = Vtable {
        clone: |p| {
            let owner = ManuallyDrop::new(Self { pointer: p.cast() });
            std::mem::forget(Self::clone(&owner));
        },
        drop: |p| drop(Self { pointer: p.cast() }),
        bytes: |p| {
            let owner = ManuallyDrop::new(Self { pointer: p.cast() });
            let bytes = owner.as_ref();
            (bytes.as_ptr(), bytes.len())
        },
        owner: |p, kind, acquire| {
            let owner = ManuallyDrop::new(Self { pointer: p.cast() });
            owner.charge_owner(kind, acquire);
            owner.owner_reference(kind, acquire);
        },
        reference: |p, cache, acquire| {
            let owner = ManuallyDrop::new(Self { pointer: p.cast() });
            owner.reference(cache, acquire);
        },
        release_scope: |p| {
            let owner = ManuallyDrop::new(Self { pointer: p.cast() });
            owner.begin_capacity_release()
        },
    };
    /// Erases the source type without changing its allocation or strong count.
    pub fn into_source(self) -> ChargedSource {
        let result = ChargedSource {
            pointer: self.pointer.cast(),
            vtable: &Self::SOURCE_VTABLE,
        };
        std::mem::forget(self);
        result
    }
}
impl ChargedSource {
    pub(crate) fn begin_capacity_release(&self) -> crate::storage::CapacityReleaseGuard {
        unsafe { (self.vtable.release_scope)(self.pointer) }
    }
}
impl Clone for ChargedSource {
    fn clone(&self) -> Self {
        unsafe {
            (self.vtable.clone)(self.pointer);
        }
        Self {
            pointer: self.pointer,
            vtable: self.vtable,
        }
    }
}
impl Drop for ChargedSource {
    fn drop(&mut self) {
        unsafe {
            (self.vtable.drop)(self.pointer);
        }
    }
}
impl AsRef<[u8]> for ChargedSource {
    fn as_ref(&self) -> &[u8] {
        // The source remains strongly held throughout this borrow. Dispatch only
        // returns the pointer/length; the reference lifetime is tied to self here.
        let (pointer, length) = unsafe { (self.vtable.bytes)(self.pointer) };
        unsafe { std::slice::from_raw_parts(pointer, length) }
    }
}
impl SharedSource for ChargedSource {
    fn owner_reference(&self, kind: OwnerKind, acquire: bool) {
        unsafe {
            (self.vtable.owner)(self.pointer, kind, acquire);
        }
    }
    fn reference(&self, cache: bool, acquire: bool) {
        // Historical payload counters exclude the source control allocation.
        unsafe {
            (self.vtable.reference)(self.pointer, cache, acquire);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ResourceCategory, SharedBytes};

    #[test]
    fn slice_owner_removal_is_protected_until_backing_release() {
        use crate::storage::{BudgetLimits, CapacityWaitStatus, Reservation};
        struct Source(Reservation);
        impl AsRef<[u8]> for Source { fn as_ref(&self) -> &[u8] { &[] } }
        impl SharedSource for Source {
            fn owner_reference(&self, owner: OwnerKind, acquire: bool) {
                self.0.owner_reference(owner, acquire);
                if !acquire {
                    assert_eq!(self.0.domain().retry_status(4096).unwrap(), CapacityWaitStatus::Waiting);
                }
            }
        }
        let domain = crate::storage::BudgetRef::new(BudgetLimits {total: (65536) + crate::storage::BudgetRef::allocation_size(), block: 65536, retained: 0}).unwrap();
        let source = ChargedShared::new(Source(domain.reserve(4096).unwrap()), &domain,
            ResourceCategory::Input).unwrap().into_source();
        let slice = SharedBytes::charged_external(source, 0..0);
        let fixed = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
        drop(slice);
        assert_eq!(domain.retry_status(4096).unwrap(), CapacityWaitStatus::Ready);
        drop(fixed);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    struct Inline([u8; 8]);
    impl AsRef<[u8]> for Inline {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    impl SharedSource for Inline {}
    #[test]
    fn erased_slices_retain_control_and_unique_owners_without_allocating() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let root = ChargedShared::new(
            Inline([1, 2, 3, 4, 5, 6, 7, 8]),
            &domain,
            ResourceCategory::Input,
        )
        .unwrap();
        let bytes = root.allocation_bytes() as u64;
        let mut source = SharedBytes::charged_external(root.clone().into_source(), 1..7);
        source.set_owner(OwnerKind::Parser);
        let lease = source.slice(1..4).clone_for(OwnerKind::Lease);
        let second = lease.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 1);
        assert_eq!(domain.ownership_statistics().bytes, [bytes, 0, 0, 0, bytes]);
        assert_eq!(lease.as_ref(), &[3, 4, 5]);
        drop(root);
        drop(source);
        assert_eq!(domain.ownership_statistics().externally_releasable, bytes);
        drop(lease);
        assert_eq!(second.as_ref(), &[3, 4, 5]);
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn erased_source_prevents_unique_mutation_and_survives_other_thread() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut root =
            ChargedShared::new(Inline([7; 8]), &domain, ResourceCategory::Input).unwrap();
        let source = root.clone().into_source();
        assert!(root.get_mut().is_none());
        std::thread::spawn(move || {
            for _ in 0..1000 {
                assert_eq!(source.clone().as_ref(), &[7; 8]);
            }
        })
        .join()
        .unwrap();
        assert!(root.get_mut().is_some());
        drop(root);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
