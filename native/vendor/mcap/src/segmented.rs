//! A radix directory keeps both payload pages and directory allocations bounded.
use crate::charged::ChargedBox;
use crate::storage::{Reservation, ResourceCategory, StorageFailure};
use std::{
    io,
    ops::{Index, IndexMut},
};
const FANOUT: usize = 64;
enum Contents<T> {
    Branch([Option<ChargedBox<Node<T>>>; FANOUT]),
    Leaf(Leaf<T>),
}
struct Leaf<T> {
    values: Vec<T>,
    charge: Reservation,
}
struct Node<T> {
    contents: Contents<T>,
}
impl<T> Node<T> {
    fn owner_reference(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        match &self.contents {
            Contents::Leaf(leaf) => leaf.charge.owner_reference(kind, acquire),
            Contents::Branch(children) => {
                for child in children.iter().flatten() {
                    child.charge_owner(kind, acquire);
                    child.owner_reference(kind, acquire);
                }
            }
        }
    }

    fn new(
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
        leaf_capacity: Option<usize>,
        owner: Option<crate::storage::OwnerKind>,
    ) -> Result<ChargedBox<Self>,StorageFailure> {
        let contents = if let Some(capacity) = leaf_capacity {
            let (values, charge) = crate::charged::vector_fixed(domain, category, capacity)?;
            if let Some(owner) = owner {
                charge.owner_reference(owner, true);
            }
            Contents::Leaf(Leaf { values, charge })
        } else {
            Contents::Branch(std::array::from_fn(|_| None))
        };
        let node = ChargedBox::new_fixed(Self { contents }, domain, category)?;
        if let Some(owner) = owner {
            node.charge_owner(owner, true);
        }
        Ok(node)
    }
    fn leaf(&self, page: usize, depth: usize) -> Option<&Vec<T>> {
        match &self.contents {
            Contents::Leaf(v) => Some(&v.values),
            Contents::Branch(children) => children[(page >> (depth * 6)) & 63]
                .as_ref()?
                .leaf(page, depth.saturating_sub(1)),
        }
    }
    fn leaf_mut(&mut self, page: usize, depth: usize) -> Option<&mut Vec<T>> {
        match &mut self.contents {
            Contents::Leaf(v) => Some(&mut v.values),
            Contents::Branch(children) => children[(page >> (depth * 6)) & 63]
                .as_mut()?
                .leaf_mut(page, depth.saturating_sub(1)),
        }
    }
    fn ensure(
        &mut self,
        page: usize,
        depth: usize,
        capacity: usize,
        domain: &crate::storage::BudgetRef,
        category: ResourceCategory,
        owner: Option<crate::storage::OwnerKind>,
    ) -> Result<(),StorageFailure> {
        if let Contents::Branch(children) = &mut self.contents {
            let slot = &mut children[(page >> (depth * 6)) & 63];
            if slot.is_none() {
                let mut child = Self::new(
                    domain,
                    category,
                    if depth == 0 { Some(capacity) } else { None },
                    owner,
                )?;
                if depth > 0 {
                    child.ensure(page, depth - 1, capacity, domain, category, owner)?;
                }
                *slot = Some(child);
            } else if depth > 0 {
                slot.as_mut().unwrap().ensure(
                    page,
                    depth - 1,
                    capacity,
                    domain,
                    category,
                    owner,
                )?;
            }
        }
        Ok(())
    }
}
pub struct BudgetedSegmentedVec<T> {
    root: Option<ChargedBox<Node<T>>>,
    domain: crate::storage::BudgetRef,
    category: ResourceCategory,
    len: usize,
    per_page: usize,
    depth: usize,
    owner: Option<crate::storage::OwnerKind>,
}
impl<T> BudgetedSegmentedVec<T> {
    /// Pins every allocation in an immutable published container. All references
    /// must be released before mutation or destruction.
    pub fn owner_reference(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        if let Some(root) = &self.root {
            root.charge_owner(kind, acquire);
            root.owner_reference(kind, acquire);
        }
    }

    pub fn new(domain: crate::storage::BudgetRef, category: ResourceCategory) -> Self {
        Self::with_page_capacity(domain, category, usize::MAX)
    }
    pub fn with_page_capacity(
        domain: crate::storage::BudgetRef,
        category: ResourceCategory,
        capacity: usize,
    ) -> Self {
        let per_page = Self::page_capacity(capacity);
        Self {
            root: None,
            domain,
            category,
            len: 0,
            per_page,
            depth: 0,
            owner: None,
        }
    }
    fn page_capacity(capacity: usize) -> usize {
        (65536 / std::mem::size_of::<T>().max(1)).max(1).min(capacity.max(1))
    }
    /// Exact physical capacity of a fresh container after reserve(count),
    /// including every radix directory and leaf control. Does not allocate.
    pub fn fresh_reservation_bytes(count: usize, page_capacity: usize) -> io::Result<usize> {
        if count == 0 { return Ok(0); }
        let per_page = Self::page_capacity(page_capacity);
        let pages = count.div_ceil(per_page);
        let leaf_bytes = std::alloc::Layout::array::<T>(per_page)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?.size();
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        let mut nodes = pages;
        // Even one leaf has a branch root. Each successive branch level groups
        // at most FANOUT children; the final single branch is the root.
        let mut level = pages;
        loop {
            level = level.div_ceil(FANOUT);
            nodes = nodes.checked_add(level).ok_or_else(invalid)?;
            if level == 1 { break; }
        }
        pages.checked_mul(leaf_bytes)
            .and_then(|bytes| nodes.checked_mul(ChargedBox::<Node<T>>::allocation_size()).and_then(|controls| bytes.checked_add(controls)))
            .ok_or_else(invalid)
    }
    pub fn allocated_bytes(&self) -> usize {
        fn count<T>(node: &ChargedBox<Node<T>>) -> usize {
            node.allocation_bytes()
                + match &node.contents {
                    Contents::Leaf(leaf) => leaf.charge.bytes(),
                    Contents::Branch(children) => {
                        children.iter().flatten().map(|c| count(c)).sum::<usize>()
                    }
                }
        }
        self.root.as_ref().map_or(0, |n| count(n))
    }
    pub fn binary_search_by_key<K: Ord>(
        &self,
        key: &K,
        f: impl Fn(&T) -> K,
    ) -> Result<usize, usize> {
        let (mut low, mut high) = (0, self.len);
        while low < high {
            let mid = low + (high - low) / 2;
            match f(&self[mid]).cmp(key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }
        Err(low)
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        self.root
            .as_ref()?
            .leaf(index / self.per_page, self.depth)?
            .get(index % self.per_page)
    }
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        self.root
            .as_mut()?
            .leaf_mut(index / self.per_page, self.depth)?
            .get_mut(index % self.per_page)
    }
    pub fn reserve(&mut self, count: usize) -> io::Result<()> {
        self.reserve_fixed(count).map_err(StorageFailure::into_io)
    }
    pub fn reserve_fixed(&mut self, count: usize) -> Result<(),StorageFailure> {
        if count == 0 {
            return Ok(());
        }
        for page in 0..=(count - 1) / self.per_page {
            self.ensure_page(page)?;
        }
        Ok(())
    }
    fn ensure_page(&mut self, page: usize) -> Result<(),StorageFailure> {
        if self.root.is_none() {
            let mut root = Node::new(&self.domain, self.category, None, self.owner)?;
            root.ensure(
                page,
                self.depth,
                self.per_page,
                &self.domain,
                self.category,
                self.owner,
            )?;
            self.root = Some(root);
            return Ok(());
        }
        if page.checked_shr(((self.depth + 1) * 6) as u32).unwrap_or(0) > 0 {
            let mut root = Node::new(&self.domain, self.category, None, self.owner)?;
            root.ensure(
                page,
                self.depth + 1,
                self.per_page,
                &self.domain,
                self.category,
                self.owner,
            )?;
            if let Contents::Branch(children) = &mut root.contents {
                children[0] = self.root.take();
            }
            self.root = Some(root);
            self.depth += 1;
        }
        self.root.as_mut().unwrap().ensure(
            page,
            self.depth,
            self.per_page,
            &self.domain,
            self.category,
            self.owner,
        )
    }
    pub fn push(&mut self, value: T) -> io::Result<()> {
        self.push_fixed(value).map_err(StorageFailure::into_io)
    }
    pub fn push_fixed(&mut self, value: T) -> Result<(),StorageFailure> {
        let next_len=self.len.checked_add(1).ok_or_else(||StorageFailure::at(
            &self.domain,self.category,usize::MAX,"segmented length",crate::storage::StorageFailureKind::Overflow))?;
        let page = self.len / self.per_page;
        self.ensure_page(page)?;
        self.root
            .as_mut()
            .unwrap()
            .leaf_mut(page, self.depth)
            .unwrap()
            .push(value);
        self.len = next_len;
        Ok(())
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        (0..self.len).map(|i| self.get(i).unwrap())
    }
    pub fn clear(&mut self) {
        self.root = None;
        self.len = 0;
        self.depth = 0;
    }
    fn swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let a = self.get_mut(a).unwrap() as *mut T;
        let b = self.get_mut(b).unwrap() as *mut T;
        unsafe {
            std::ptr::swap(a, b);
        }
    }
    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            let index = self.len - 1;
            self.root
                .as_mut()
                .unwrap()
                .leaf_mut(index / self.per_page, self.depth)
                .unwrap()
                .pop();
            self.len -= 1;
        }
    }
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let index = self.len - 1;
        let value = self
            .root
            .as_mut()
            .unwrap()
            .leaf_mut(index / self.per_page, self.depth)
            .unwrap()
            .pop();
        self.len -= 1;
        value
    }
    pub fn remove(&mut self, index: usize) -> T {
        assert!(index < self.len);
        for i in index + 1..self.len {
            self.swap(i - 1, i);
        }
        self.pop().unwrap()
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let mut write = 0;
        for read in 0..self.len {
            if keep(&self[read]) {
                self.swap(write, read);
                write += 1;
            }
        }
        self.truncate(write);
    }
    pub fn remove_prefix(&mut self, n: usize) {
        assert!(n <= self.len);
        for i in n..self.len {
            self.swap(i - n, i);
        }
        self.truncate(self.len - n);
    }
    pub fn reverse_range(&mut self, start: usize) {
        for i in 0..(self.len - start) / 2 {
            self.swap(start + i, self.len - 1 - i);
        }
    }
    pub fn sort_by_key<K: Ord>(&mut self, key: impl Fn(&T) -> K) {
        self.sort_range_by_key(0, key);
    }
    pub fn sort_range_by_key<K: Ord>(&mut self, start: usize, key: impl Fn(&T) -> K) {
        self.sort_range_by(start, |a,b| key(a).cmp(&key(b)));
    }
    pub fn sort_by(&mut self, compare: impl Fn(&T,&T)->std::cmp::Ordering) {
        self.sort_range_by(0, compare);
    }
    fn sort_range_by(&mut self, start: usize, compare: impl Fn(&T,&T)->std::cmp::Ordering) {
        fn sift<T>(
            v: &mut BudgetedSegmentedVec<T>,
            mut root: usize,
            end: usize,
            start: usize,
            compare: &impl Fn(&T,&T)->std::cmp::Ordering,
        ) {
            loop {
                let mut child = root * 2 + 1;
                if child >= end {
                    break;
                }
                if child + 1 < end && compare(&v[start + child], &v[start + child + 1]).is_lt() {
                    child += 1;
                }
                if !compare(&v[start + root], &v[start + child]).is_lt() {
                    break;
                }
                v.swap(start + root, start + child);
                root = child;
            }
        }
        for i in (0..(self.len - start) / 2).rev() {
            sift(self, i, self.len - start, start, &compare);
        }
        for end in (1..self.len - start).rev() {
            self.swap(start, start + end);
            sift(self, 0, end, start, &compare);
        }
    }
}
impl<T> Index<usize> for BudgetedSegmentedVec<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        self.get(i).expect("index")
    }
}
impl<T> IndexMut<usize> for BudgetedSegmentedVec<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        self.get_mut(i).expect("index")
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn fresh_capacity_matches_actual_layouts_across_radix_depths() {
        for page_capacity in [1, 7, 256, usize::MAX] {
            for count in [0, 1, 63, 64, 65, 4095, 4096, 4097, 65536] {
                let domain=crate::storage::BudgetRef::new(Default::default()).unwrap();
                let before=domain.workload_detailed_statistics();
                let required=BudgetedSegmentedVec::<u64>::fresh_reservation_bytes(count,page_capacity).unwrap();
                assert_eq!(domain.workload_detailed_statistics().allocation_count,before.allocation_count);
                let mut values=BudgetedSegmentedVec::<u64>::with_page_capacity(domain.clone(),ResourceCategory::Index,page_capacity);
                values.reserve(count).unwrap();
                assert_eq!(values.allocated_bytes(),required,"count={count}, page_capacity={page_capacity}");
                assert_eq!(domain.workload_statistics().current,required as u64);
                assert_eq!(domain.workload_detailed_statistics().allocated_bytes,required as u64);
                drop(values);
                assert_eq!(domain.workload_statistics().current,0);
            }
        }
        assert!(BudgetedSegmentedVec::<u64>::fresh_reservation_bytes(usize::MAX,usize::MAX).is_err());
    }
    #[test]
    fn every_first_page_failure_restores_empty_state_and_charge() {
        for refusal in 0..3 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut values = super::BudgetedSegmentedVec::with_page_capacity(
                domain.clone(),
                crate::storage::ResourceCategory::Index,
                1,
            );
            domain.fail_allocation_at(refusal);
            assert!(values.push(7u64).is_err());
            assert_eq!(values.len(), 0);
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
    #[test]
    fn directory_growth_failures_preserve_previous_pages() {
        for refusal in 0..4 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut values = super::BudgetedSegmentedVec::with_page_capacity(
                domain.clone(),
                crate::storage::ResourceCategory::Index,
                1,
            );
            for i in 0..64u64 {
                values.push(i).unwrap();
            }
            let before = domain.workload_statistics().current;
            domain.fail_allocation_at(refusal);
            assert!(values.push(64).is_err());
            assert_eq!(values.len(), 64);
            for i in 0..64 {
                assert_eq!(values[i], i as u64);
            }
            assert_eq!(domain.workload_statistics().current, before);
            drop(values);
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
    #[test]
    fn descriptor_page_and_directory_are_distinct_actual_allocations() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut values = super::BudgetedSegmentedVec::with_page_capacity(
            domain.clone(),
            crate::storage::ResourceCategory::Descriptor,
            3,
        );
        values.push(42u64).unwrap();
        let stats = domain.workload_detailed_statistics();
        assert_eq!(stats.allocation_count, 3);
        assert_eq!(stats.allocated_bytes, values.allocated_bytes() as u64);
        values.owner_reference(crate::storage::OwnerKind::Cache, true);
        assert_eq!(
            domain.ownership_statistics().immediately_reclaimable,
            values.allocated_bytes() as u64
        );
        values.owner_reference(crate::storage::OwnerKind::Cache, false);
    }

    use super::*;
    #[test]
    fn pages_and_radix_sort_release() {
        let budget = crate::storage::BudgetRef::new(Default::default()).unwrap();
        {
            let mut values = BudgetedSegmentedVec::with_page_capacity(
                budget.clone(),
                ResourceCategory::Index,
                7,
            );
            for i in (0..10000).rev() {
                values.push(i).unwrap();
            }
            values.sort_by_key(|v| *v);
            for i in 0..10000 {
                assert_eq!(values[i], i);
            }
        }
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn million_descriptors_use_bounded_pages() {
        let budget = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: (16 * 1024 * 1024) + crate::storage::BudgetRef::allocation_size(),
            block: 65536,
            retained: 0,
        }).unwrap();
        let mut values = BudgetedSegmentedVec::new(budget.clone(), ResourceCategory::Index);
        for i in 0..1_000_000u64 {
            values.push(i).unwrap();
        }
        assert_eq!(values[999999], 999999);
        assert!(values.allocated_bytes() < 9 * 1024 * 1024);
        assert_eq!(values.allocated_bytes() as u64, budget.workload_statistics().current);
        values.clear();
        assert_eq!(budget.workload_statistics().current, 0);
    }
    #[test]
    fn refused_page_preserves_prefix() {
        let budget = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: (4096) + crate::storage::BudgetRef::allocation_size(),
            block: 4096,
            retained: 0,
        }).unwrap();
        let mut values =
            BudgetedSegmentedVec::with_page_capacity(budget, ResourceCategory::Index, 1);
        let mut n = 0;
        while values.push(n).is_ok() {
            n += 1;
        }
        assert!(n > 0);
        assert_eq!(values.len(), n);
        for i in 0..n {
            assert_eq!(values[i], i);
        }
    }
}

/// Immutable after cloning; summary/index owners share the page tree.
pub struct SharedSegmentedVec<T> {
    domain: crate::storage::BudgetRef,
    root: Option<crate::charged::ChargedTree<BudgetedSegmentedVec<T>>>,
    empty: BudgetedSegmentedVec<T>,
    owner: crate::storage::OwnerKind,
}
impl<T> crate::charged::TreeOwnership for BudgetedSegmentedVec<T> {
    fn reference_children(&self, kind: crate::storage::OwnerKind, acquire: bool) {
        self.owner_reference(kind, acquire);
    }
    fn mutation_owner(&mut self, owner: Option<crate::storage::OwnerKind>) {
        self.owner = owner;
    }
}
impl<T> SharedSegmentedVec<T> {
    pub fn new(domain: crate::storage::BudgetRef) -> Self {
        Self::new_owned(domain, crate::storage::OwnerKind::Parser)
    }
    pub(crate) fn new_owned(domain: crate::storage::BudgetRef, owner: crate::storage::OwnerKind) -> Self {
        Self {
            empty: BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Index),
            domain,
            root: None,
            owner,
        }
    }
    pub fn push(&mut self, value:T) -> io::Result<()> {
        self.push_fixed(value).map_err(|error|match error {
            crate::McapError::Storage(failure)=>failure.into_io(),
            crate::McapError::StaticIoError(message)=>io::Error::other(message),
            other=>io::Error::other(other),
        })
    }
    pub fn push_fixed(&mut self, value:T) -> crate::McapResult<()> {
        if self.root.is_none() {
            let mut root=crate::charged::ChargedTree::new_owned_fixed(
                BudgetedSegmentedVec::new(self.domain.clone(),ResourceCategory::Index),
                &self.domain,ResourceCategory::Index,self.owner)?;
            root.get_mut().unwrap().push_fixed(value)?;
            self.root=Some(root);
            return Ok(());
        }
        Ok(self.root.as_mut().unwrap().get_mut()
            .ok_or(crate::McapError::StaticIoError("Shared index is immutable"))?
            .push_fixed(value)?)
    }
    pub fn iter(&self) -> SegmentIter<'_, T> {
        SegmentIter {
            data: self,
            position: 0,
        }
    }
}
#[cfg(test)]
mod shared_transaction_tests {
    use super::*;

    #[test]
    fn shared_pages_keep_pins_across_radix_growth_and_owner_transfer() {
        use crate::storage::OwnerKind;
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut root = crate::charged::ChargedTree::new_owned(
            BudgetedSegmentedVec::with_page_capacity(domain.clone(), ResourceCategory::Index, 2),
            &domain,
            ResourceCategory::Index,
            OwnerKind::Operation,
        )
        .unwrap();
        for value in 0..130u64 {
            root.get_mut().unwrap().push(value).unwrap();
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
                domain.workload_statistics().current
            );
            assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        }
        let mut cache = root.clone_with_owner(OwnerKind::Cache);
        drop(root);
        assert_eq!(
            domain.ownership_statistics().immediately_reclaimable,
            domain.workload_statistics().current
        );
        cache.get_mut().unwrap().push(130).unwrap();
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Cache as usize],
            domain.workload_statistics().current
        );
        let operation = cache.clone_with_owner(OwnerKind::Operation);
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        drop(operation);
        assert_eq!(
            domain.ownership_statistics().immediately_reclaimable,
            domain.workload_statistics().current
        );
        drop(cache);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn shared_growth_refusal_preserves_values_and_all_owner_pins() {
        use crate::storage::OwnerKind;
        // Growing past 64 leaves adds a radix level and a new leaf. Fail each
        // allocation before publication, retaining the previous 128 elements.
        for failure in 0..4 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut root = crate::charged::ChargedTree::new_owned(
                BudgetedSegmentedVec::with_page_capacity(
                    domain.clone(),
                    ResourceCategory::Index,
                    2,
                ),
                &domain,
                ResourceCategory::Index,
                OwnerKind::Operation,
            )
            .unwrap();
            for value in 0..128u64 {
                root.get_mut().unwrap().push(value).unwrap();
            }
            let capacity = domain.workload_statistics().current;
            domain.fail_allocation_at(failure);
            assert!(root.get_mut().unwrap().push(128).is_err());
            assert_eq!(root.len(), 128);
            assert!(root.iter().copied().eq(0..128));
            assert_eq!(domain.workload_statistics().current, capacity);
            assert_eq!(
                domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
                capacity
            );
            drop(root);
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }

    #[test]
    fn first_append_refusal_rolls_back_root_and_pages() {
        // Shared root, exact leaf storage, and radix node are independent allocations.
        for failure in 0..3 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut values = SharedSegmentedVec::new(domain.clone());
            domain.fail_allocation_at(failure);
            assert!(values.push(42u64).is_err());
            assert!(values.is_empty());
            assert!(values.root.is_none());
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
}
impl<T> Default for SharedSegmentedVec<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}
impl<T> Clone for SharedSegmentedVec<T> {
    fn clone(&self) -> Self {
        Self {
            domain: self.domain.clone(),
            root: self.root.clone(),
            empty: BudgetedSegmentedVec::new(self.domain.clone(), ResourceCategory::Index),
            owner: self.owner,
        }
    }
}
impl<T> std::ops::Deref for SharedSegmentedVec<T> {
    type Target = BudgetedSegmentedVec<T>;
    fn deref(&self) -> &Self::Target {
        self.root.as_deref().unwrap_or(&self.empty)
    }
}
impl<T: PartialEq> PartialEq for SharedSegmentedVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<T: Eq> Eq for SharedSegmentedVec<T> {}
impl<T: std::fmt::Debug> std::fmt::Debug for SharedSegmentedVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
pub struct SegmentIter<'a, T> {
    data: &'a BudgetedSegmentedVec<T>,
    position: usize,
}
impl<'a, T> Iterator for SegmentIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.data.get(self.position);
        self.position += usize::from(result.is_some());
        result
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.data.len() - self.position;
        (n, Some(n))
    }
}
impl<T> ExactSizeIterator for SegmentIter<'_, T> {}
impl<'a, T> IntoIterator for &'a SharedSegmentedVec<T> {
    type Item = &'a T;
    type IntoIter = SegmentIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a BudgetedSegmentedVec<T> {
    type Item = &'a T;
    type IntoIter = SegmentIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        SegmentIter {
            data: self,
            position: 0,
        }
    }
}
