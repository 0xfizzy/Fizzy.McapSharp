//! Fixed u16 key space with lazily allocated, exactly charged 256-entry pages.
use crate::{
    charged::ChargedBox,
    storage::{OwnerKind, ResourceCategory},
};
use std::io;
const PAGE: usize = 256;
struct Page<T> {
    entries: [Option<T>; PAGE],
    occupied: usize,
}
pub struct U16Table<T> {
    pages: [Option<ChargedBox<Page<T>>>; PAGE],
    len: usize,
    domain: crate::storage::BudgetRef,
    category: ResourceCategory,
    owner: Option<OwnerKind>,
}
impl<T> U16Table<T> {
    pub fn new(domain: crate::storage::BudgetRef, category: ResourceCategory) -> Self {
        Self {
            pages: std::array::from_fn(|_| None),
            len: 0,
            domain,
            category,
            owner: None,
        }
    }
    /// Allocate pages with the supplied owner pin before publishing entries.
    pub fn new_owned(
        domain: crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: OwnerKind,
    ) -> Self {
        let mut result = Self::new(domain, category);
        result.owner = Some(owner);
        result
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, key: &u16) -> Option<&T> {
        self.pages[*key as usize / PAGE].as_ref()?.entries[*key as usize % PAGE].as_ref()
    }
    pub fn get_mut(&mut self, key: &u16) -> Option<&mut T> {
        self.pages[*key as usize / PAGE].as_mut()?.entries[*key as usize % PAGE].as_mut()
    }
    pub fn contains_key(&self, key: &u16) -> bool {
        self.get(key).is_some()
    }
    pub fn insert(&mut self, key: u16, value: T) -> io::Result<Option<T>> {
        self.insert_fixed(key,value).map_err(crate::storage::StorageFailure::into_io)
    }
    pub fn insert_fixed(&mut self, key:u16, value:T) -> Result<Option<T>,crate::storage::StorageFailure> {
        let slot = &mut self.pages[key as usize / PAGE];
        if slot.is_none() {
            let page = ChargedBox::new_fixed(
                Page {
                    entries: std::array::from_fn(|_| None),
                    occupied: 0,
                },
                &self.domain,
                self.category,
            )?;
            if let Some(owner) = self.owner {
                page.charge_owner(owner, true);
            }
            *slot = Some(page);
        }
        let page = slot.as_mut().unwrap();
        let previous = page.entries[key as usize % PAGE].replace(value);
        if previous.is_none() {
            self.len += 1;
            page.occupied += 1;
        }
        Ok(previous)
    }
    pub fn remove(&mut self, key: &u16) -> Option<T> {
        let slot = &mut self.pages[*key as usize / PAGE];
        let page = slot.as_mut()?;
        let result = page.entries[*key as usize % PAGE].take()?;
        page.occupied -= 1;
        self.len -= 1;
        if page.occupied == 0 {
            *slot = None;
        }
        Some(result)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u16, &T)> {
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.as_ref().map(|p| (i, p)))
            .flat_map(|(i, p)| {
                p.entries
                    .iter()
                    .enumerate()
                    .filter_map(move |(j, v)| v.as_ref().map(|v| ((i * PAGE + j) as u16, v)))
            })
    }
    pub fn allocated_bytes(&self) -> usize {
        self.pages
            .iter()
            .flatten()
            .map(|p| p.allocation_bytes())
            .sum()
    }
    pub fn owner_reference(&self, kind: OwnerKind, acquire: bool) {
        for p in self.pages.iter().flatten() {
            p.charge_owner(kind, acquire);
        }
    }
}
/// A charged immutable page tree shared by summary owners. Mutation requires
/// unique ownership; clones never rebuild a directory or copy entries.
pub struct SharedU16Table<T> {
    root: Option<crate::charged::ChargedTree<U16Table<T>>>,
    empty: U16Table<T>,
    owner: OwnerKind,
}
impl<T> SharedU16Table<T> {
    pub fn new(domain: crate::storage::BudgetRef, category: ResourceCategory) -> Self {
        Self::new_owned(domain, category, OwnerKind::Parser)
    }
    pub(crate) fn new_owned(
        domain: crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: OwnerKind,
    ) -> Self {
        Self {
            root: None,
            empty: U16Table::new(domain, category),
            owner,
        }
    }
    pub fn insert(&mut self, key:u16, value:T) -> io::Result<Option<T>> {
        self.insert_fixed(key,value).map_err(|error| match error {
            crate::McapError::Storage(failure)=>failure.into_io(),
            crate::McapError::StaticIoError(message)=>io::Error::other(message),
            other=>io::Error::other(other),
        })
    }
    pub fn insert_fixed(&mut self, key:u16, value:T) -> crate::McapResult<Option<T>> {
        if let Some(root) = &mut self.root {
            return Ok(root.get_mut()
                .ok_or(crate::McapError::StaticIoError("Shared declaration table is immutable"))?
                .insert_fixed(key,value)?);
        }
        let mut root=crate::charged::ChargedTree::new_owned_fixed(
            U16Table::new(self.empty.domain.clone(),self.empty.category),
            &self.empty.domain,self.empty.category,self.owner)?;
        let previous=root.get_mut().unwrap().insert_fixed(key,value)?;
        self.root=Some(root);
        Ok(previous)
    }
    pub fn keys(&self) -> impl Iterator<Item = u16> + '_ {
        self.iter().map(|(key, _)| key)
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }
}
impl<T> Clone for SharedU16Table<T> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            empty: U16Table::new(self.empty.domain.clone(), self.empty.category),
            owner: self.owner,
        }
    }
}
impl<T> Default for SharedU16Table<T> {
    fn default() -> Self {
        Self::new(Default::default(), ResourceCategory::Declaration)
    }
}
impl<T> std::ops::Deref for SharedU16Table<T> {
    type Target = U16Table<T>;
    fn deref(&self) -> &Self::Target {
        self.root.as_deref().unwrap_or(&self.empty)
    }
}
impl<T> std::ops::Index<&u16> for SharedU16Table<T> {
    type Output = T;
    fn index(&self, key: &u16) -> &T {
        self.get(key).expect("Missing declaration ID")
    }
}
impl<T: PartialEq> PartialEq for SharedU16Table<T> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<T: Eq> Eq for SharedU16Table<T> {}
impl<T: std::fmt::Debug> std::fmt::Debug for SharedU16Table<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
pub struct IntoIter<T> {
    table: U16Table<T>,
    page: usize,
    entry: usize,
}
impl<T> IntoIterator for U16Table<T> {
    type Item = (u16, T);
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            table: self,
            page: 0,
            entry: 0,
        }
    }
}
impl<T> Iterator for IntoIter<T> {
    type Item = (u16, T);
    fn next(&mut self) -> Option<Self::Item> {
        while self.page < PAGE {
            let slot = &mut self.table.pages[self.page];
            if let Some(page) = slot {
                while self.entry < PAGE {
                    let key = (self.page * PAGE + self.entry) as u16;
                    let value = page.entries[self.entry].take();
                    self.entry += 1;
                    if let Some(value) = value {
                        page.occupied -= 1;
                        self.table.len -= 1;
                        if page.occupied == 0 {
                            *slot = None;
                        }
                        return Some((key, value));
                    }
                }
            }
            self.page += 1;
            self.entry = 0;
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.table.len, Some(self.table.len))
    }
}
impl<T> ExactSizeIterator for IntoIter<T> {}
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::*;
    #[test]
    fn operation_table_clones_and_new_pages_keep_operation_ownership() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let empty = SharedU16Table::<u64>::new_owned(
            domain.clone(),
            ResourceCategory::Index,
            OwnerKind::Operation,
        );
        let mut table = empty.clone();
        drop(empty);
        for key in [0, 256, 65535] {
            table.insert(key, key as u64).unwrap();
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
                0
            );
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
                domain.workload_statistics().current
            );
        }
        let before = domain.workload_detailed_statistics().allocation_count;
        let mut retained = table.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, before);
        drop(table);
        retained.insert(512, 42).unwrap();
        assert_eq!(retained[&65535], 65535);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            domain.workload_statistics().current
        );
        drop(retained);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn operation_table_failed_publication_rolls_back_root_and_page() {
        for failure in 0..2 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut table = SharedU16Table::new_owned(
                domain.clone(),
                ResourceCategory::Index,
                OwnerKind::Operation,
            );
            domain.fail_allocation_at(failure);
            assert!(table.insert(1, 7u64).is_err());
            assert!(table.is_empty());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    #[test]
    fn shared_root_and_pages_have_unique_owner_capacity() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = SharedU16Table::new(domain.clone(), ResourceCategory::Declaration);
        table.insert(1, 7u64).unwrap();
        table.insert(256, 8).unwrap();
        let capacity = domain.workload_statistics().current;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            capacity
        );
        let cache = table
            .root
            .as_ref()
            .unwrap()
            .clone_with_owner(OwnerKind::Cache);
        let second_cache = cache.clone();
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Cache as usize],
            capacity
        );
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        drop(table);
        assert_eq!(
            domain.ownership_statistics().immediately_reclaimable,
            capacity
        );
        let operation = cache.clone_with_owner(OwnerKind::Operation);
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        drop(cache);
        drop(second_cache);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            capacity
        );
        drop(operation);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn mutation_keeps_existing_and_new_pages_pinned_before_return() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = SharedU16Table::new(domain.clone(), ResourceCategory::Declaration);
        table.insert(1, 7u64).unwrap();
        for key in [256, 512, 65535] {
            let mut guard = table.root.as_mut().unwrap().get_mut().unwrap();
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
                domain.workload_statistics().current
            );
            guard.insert(key, key as u64).unwrap();
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
                domain.workload_statistics().current
            );
        }
        // After another owner has left, subsequent mutations still inherit the
        // remaining owner's kind rather than the original parser's kind.
        let mut operation = table
            .root
            .as_ref()
            .unwrap()
            .clone_with_owner(OwnerKind::Operation);
        drop(table);
        {
            let mut guard = operation.get_mut().unwrap();
            guard.insert(1024, 42).unwrap();
            guard.remove(&256);
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
                domain.workload_statistics().current
            );
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
                0
            );
        }
        drop(operation);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn mutation_refusal_and_unwind_restore_page_owners() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = SharedU16Table::new(domain.clone(), ResourceCategory::Declaration);
        table.insert(1, 7u64).unwrap();
        let capacity = domain.workload_statistics().current;
        domain.fail_allocation_at(0);
        assert!(table.insert(256, 8).is_err());
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            capacity
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = table.root.as_mut().unwrap().get_mut().unwrap();
            guard.remove(&1);
            panic!("injected mutation panic");
        }));
        assert!(result.is_err());
        assert!(table.is_empty());
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            domain.workload_statistics().current
        );
        drop(table);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn shared_directory_refusal_and_last_owner_release() {
        for failure in 0..2 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut table = SharedU16Table::new(domain.clone(), ResourceCategory::Declaration);
            domain.fail_allocation_at(failure);
            assert!(table.insert(1, 7).is_err());
            assert!(table.is_empty());
            assert_eq!(domain.workload_statistics().current, 0);
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = SharedU16Table::new(domain.clone(), ResourceCategory::Declaration);
        table.insert(255, 7).unwrap();
        table.insert(256, 8).unwrap();
        let before = domain.workload_detailed_statistics();
        let other = table.clone();
        assert_eq!(
            domain.workload_detailed_statistics().allocation_count,
            before.allocation_count
        );
        assert!(table.insert(257, 9).is_err());
        assert_eq!(table.len(), 2);
        drop(table);
        assert_eq!(other[&256], 8);
        assert!(domain.workload_statistics().current > 0);
        drop(other);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn consuming_iterator_partial_drop_releases_every_value_and_page() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Tracked(Arc<AtomicUsize>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut table = U16Table::new(domain.clone(), ResourceCategory::Declaration);
        for key in [0, 1, 256, 65535] {
            table.insert(key, Tracked(drops.clone())).unwrap();
        }
        let mut iter = table.into_iter();
        assert_eq!(iter.len(), 4);
        let (key, value) = iter.next().unwrap();
        assert_eq!(key, 0);
        assert_eq!(iter.len(), 3);
        drop(iter);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(value);
        assert_eq!(drops.load(Ordering::Relaxed), 4);
    }
    #[test]
    fn all_keys_iterate_in_order_and_empty_pages_release() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = U16Table::new(domain.clone(), ResourceCategory::Declaration);
        for key in (0..=u16::MAX).rev() {
            assert_eq!(table.insert(key, key as u32).unwrap(), None);
        }
        assert_eq!(table.len(), 65536);
        for (position, (key, value)) in table.iter().enumerate() {
            assert_eq!(position, key as usize);
            assert_eq!(*value, key as u32);
        }
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 256);
        assert_eq!(domain.workload_statistics().current, table.allocated_bytes() as u64);
        for key in 0..=u16::MAX {
            assert_eq!(table.remove(&key), Some(key as u32));
        }
        assert!(table.is_empty());
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn page_refusal_preserves_existing_values_and_charge() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut table = U16Table::new(domain.clone(), ResourceCategory::Index);
        table.insert(255, 7).unwrap();
        let charged = domain.workload_statistics().current;
        domain.fail_allocation_at(0);
        assert!(table.insert(256, 8).is_err());
        assert_eq!(table.get(&255), Some(&7));
        assert_eq!(table.get(&256), None);
        assert_eq!(table.len(), 1);
        assert_eq!(domain.workload_statistics().current, charged);
        assert_eq!(table.insert(255, 9).unwrap(), Some(7));
        assert_eq!(domain.workload_statistics().current, charged);
    }
}

impl<T> crate::charged::TreeOwnership for U16Table<T> {
    fn reference_children(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        self.owner_reference(kind, acquire);
    }
    fn mutation_owner(&mut self, owner: Option<OwnerKind>) {
        self.owner = owner;
    }
}
