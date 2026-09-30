//! Explicit, fallible allocation with a reservation that outlives the storage.
use crate::storage::{MemoryBudget, Reservation, ResourceCategory};
use std::{
    alloc::{alloc, dealloc, Layout},
    io,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::Arc,
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
    pub fn new(
        value: T,
        domain: &Arc<MemoryBudget>,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        let layout = Layout::new::<Entry<T>>();
        let mut charge = domain.reserve_class(layout.size(), category)?;
        domain.allocation_attempt()?;
        let pointer = NonNull::new(unsafe { alloc(layout) }.cast::<Entry<T>>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        charge.commit(layout.size());
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
            let release = Release {
                pointer: self.pointer.cast(),
                layout: Layout::new::<Entry<T>>(),
                charge: Some(ManuallyDrop::take(&mut entry.charge)),
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
    fn charges_actual_layout_and_releases() {
        let domain = Arc::new(MemoryBudget::default());
        let value = ChargedBox::new(42u64, &domain, ResourceCategory::Scratch).unwrap();
        assert_eq!(*value, 42);
        assert_eq!(
            domain.statistics().current,
            Layout::new::<Entry<u64>>().size() as u64
        );
        drop(value);
        assert_eq!(domain.statistics().current, 0);
    }
    #[test]
    fn refusal_does_not_allocate_or_publish() {
        let domain = Arc::new(MemoryBudget::new(crate::storage::BudgetLimits {
            total: 1,
            block: 1,
            retained: 0,
        }));
        assert!(ChargedBox::new(42u64, &domain, ResourceCategory::Scratch).is_err());
        assert_eq!(domain.statistics().current, 0);
        assert_eq!(domain.detailed_statistics().allocation_count, 0);
    }
}

/// Allocates exactly the requested byte capacity; the owner must drop data before its charge.
pub fn bytes(
    domain: &Arc<MemoryBudget>,
    category: ResourceCategory,
    capacity: usize,
) -> io::Result<(Vec<u8>, Reservation)> {
    let mut charge = domain.reserve_class(capacity, category)?;
    if capacity == 0 {
        return Ok((Vec::new(), charge));
    }
    let layout =
        Layout::array::<u8>(capacity).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    domain.allocation_attempt()?;
    let raw = unsafe { alloc(layout) };
    if raw.is_null() {
        return Err(io::ErrorKind::OutOfMemory.into());
    }
    charge.commit(capacity);
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
    pub fn new(
        value: T,
        domain: &Arc<MemoryBudget>,
        category: ResourceCategory,
    ) -> io::Result<Self> {
        let allocation = ChargedBox::new(
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
    fn concurrent_owners_retain_one_charge() {
        let domain = Arc::new(MemoryBudget::default());
        let mut root = ChargedShared::new(42usize, &domain, ResourceCategory::Index).unwrap();
        let bytes = domain.statistics().current;
        let other = root.clone();
        assert!(root.get_mut().is_none());
        std::thread::spawn(move || {
            for _ in 0..1000 {
                assert_eq!(*other.clone(), 42);
            }
        })
        .join()
        .unwrap();
        assert_eq!(domain.statistics().current, bytes);
        *root.get_mut().unwrap() = 43;
        assert_eq!(*root, 43);
        drop(root);
        assert_eq!(domain.statistics().current, 0);
    }
    #[test]
    fn destructor_panic_still_returns_storage_charge() {
        struct PanicDrop;
        impl Drop for PanicDrop {
            fn drop(&mut self) {
                panic!("injected destructor failure");
            }
        }
        let domain = Arc::new(MemoryBudget::default());
        let value = ChargedBox::new(PanicDrop, &domain, ResourceCategory::Scratch).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).is_err());
        assert_eq!(domain.statistics().current, 0);
    }
}
