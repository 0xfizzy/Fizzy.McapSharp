//! A radix directory keeps both payload pages and directory allocations bounded.
use crate::storage::{MemoryBudget, Reservation, ResourceCategory};
use std::{
    alloc::{alloc, Layout},
    io,
    ops::{Index, IndexMut},
    sync::Arc,
};
const FANOUT: usize = 64;
enum Contents<T> {
    Branch([Option<Box<Node<T>>>; FANOUT]),
    Leaf(Vec<T>),
}
struct Node<T> {
    contents: Contents<T>,
    _charge: Reservation,
}
impl<T> Node<T> {
    fn new(
        domain: &Arc<MemoryBudget>,
        category: ResourceCategory,
        leaf_capacity: Option<usize>,
    ) -> io::Result<Box<Self>> {
        let capacity = leaf_capacity.unwrap_or(0);
        let total = std::mem::size_of::<Self>()
            .checked_add(
                capacity
                    .checked_mul(std::mem::size_of::<T>())
                    .ok_or_else(|| io::Error::other("Page overflow"))?,
            )
            .ok_or_else(|| io::Error::other("Page overflow"))?;
        let mut charge = domain.reserve_class(total, category)?;
        let contents = if leaf_capacity.is_some() {
            let mut v = Vec::new();
            v.try_reserve_exact(capacity).map_err(io::Error::other)?;
            Contents::Leaf(v)
        } else {
            Contents::Branch(std::array::from_fn(|_| None))
        };
        let p = unsafe { alloc(Layout::new::<Self>()) }.cast::<Self>();
        if p.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Directory allocation failed",
            ));
        }
        charge.commit(total);
        unsafe {
            p.write(Self {
                contents,
                _charge: charge,
            });
            Ok(Box::from_raw(p))
        }
    }
    fn leaf(&self, page: usize, depth: usize) -> Option<&Vec<T>> {
        match &self.contents {
            Contents::Leaf(v) => Some(v),
            Contents::Branch(children) => children[(page >> (depth * 6)) & 63]
                .as_ref()?
                .leaf(page, depth.saturating_sub(1)),
        }
    }
    fn leaf_mut(&mut self, page: usize, depth: usize) -> Option<&mut Vec<T>> {
        match &mut self.contents {
            Contents::Leaf(v) => Some(v),
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
        domain: &Arc<MemoryBudget>,
        category: ResourceCategory,
    ) -> io::Result<()> {
        if let Contents::Branch(children) = &mut self.contents {
            let slot = &mut children[(page >> (depth * 6)) & 63];
            if slot.is_none() {
                let mut child = Self::new(
                    domain,
                    category,
                    if depth == 0 { Some(capacity) } else { None },
                )?;
                if depth > 0 {
                    child.ensure(page, depth - 1, capacity, domain, category)?;
                }
                *slot = Some(child);
            } else if depth > 0 {
                slot.as_mut()
                    .unwrap()
                    .ensure(page, depth - 1, capacity, domain, category)?;
            }
        }
        Ok(())
    }
}
pub struct BudgetedSegmentedVec<T> {
    root: Option<Box<Node<T>>>,
    domain: Arc<MemoryBudget>,
    category: ResourceCategory,
    len: usize,
    per_page: usize,
    depth: usize,
}
impl<T> BudgetedSegmentedVec<T> {
    pub fn new(domain: Arc<MemoryBudget>, category: ResourceCategory) -> Self {
        Self::with_page_capacity(domain, category, usize::MAX)
    }
    pub fn with_page_capacity(
        domain: Arc<MemoryBudget>,
        category: ResourceCategory,
        capacity: usize,
    ) -> Self {
        let per_page = (65536 / std::mem::size_of::<T>().max(1))
            .max(1)
            .min(capacity.max(1));
        Self {
            root: None,
            domain,
            category,
            len: 0,
            per_page,
            depth: 0,
        }
    }
    pub fn allocated_bytes(&self) -> usize {
        fn count<T>(node: &Node<T>) -> usize {
            node._charge.bytes()
                + match &node.contents {
                    Contents::Leaf(_) => 0,
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
        if count == 0 {
            return Ok(());
        }
        for page in 0..=(count - 1) / self.per_page {
            self.ensure_page(page)?;
        }
        Ok(())
    }
    fn ensure_page(&mut self, page: usize) -> io::Result<()> {
        if self.root.is_none() {
            self.root = Some(Node::new(&self.domain, self.category, None)?);
        }
        if page.checked_shr(((self.depth + 1) * 6) as u32).unwrap_or(0) > 0 {
            let mut root = Node::new(&self.domain, self.category, None)?;
            root.ensure(
                page,
                self.depth + 1,
                self.per_page,
                &self.domain,
                self.category,
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
        )
    }
    pub fn push(&mut self, value: T) -> io::Result<()> {
        let page = self.len / self.per_page;
        self.ensure_page(page)?;
        self.root
            .as_mut()
            .unwrap()
            .leaf_mut(page, self.depth)
            .unwrap()
            .push(value);
        self.len += 1;
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
    pub fn remove(&mut self, index: usize) {
        assert!(index < self.len);
        for i in index + 1..self.len {
            self.swap(i - 1, i);
        }
        self.truncate(self.len - 1);
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
        fn sift<T, K: Ord>(
            v: &mut BudgetedSegmentedVec<T>,
            mut root: usize,
            end: usize,
            start: usize,
            key: &impl Fn(&T) -> K,
        ) {
            loop {
                let mut child = root * 2 + 1;
                if child >= end {
                    break;
                }
                if child + 1 < end && key(&v[start + child]) < key(&v[start + child + 1]) {
                    child += 1;
                }
                if key(&v[start + root]) >= key(&v[start + child]) {
                    break;
                }
                v.swap(start + root, start + child);
                root = child;
            }
        }
        for i in (0..(self.len - start) / 2).rev() {
            sift(self, i, self.len - start, start, &key);
        }
        for end in (1..self.len - start).rev() {
            self.swap(start, start + end);
            sift(self, 0, end, start, &key);
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
    use super::*;
    #[test]
    fn pages_and_radix_sort_release() {
        let budget = Arc::new(MemoryBudget::default());
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
        assert_eq!(budget.statistics().current, 0);
    }
    #[test]
    fn million_descriptors_use_bounded_pages() {
        let budget = Arc::new(MemoryBudget::new(crate::storage::BudgetLimits {
            total: 16 * 1024 * 1024,
            block: 65536,
            retained: 0,
        }));
        let mut values = BudgetedSegmentedVec::new(budget.clone(), ResourceCategory::Index);
        for i in 0..1_000_000u64 {
            values.push(i).unwrap();
        }
        assert_eq!(values[999999], 999999);
        assert!(values.allocated_bytes() < 9 * 1024 * 1024);
        assert_eq!(values.allocated_bytes() as u64, budget.statistics().current);
        values.clear();
        assert_eq!(budget.statistics().current, 0);
    }
    #[test]
    fn refused_page_preserves_prefix() {
        let budget = Arc::new(MemoryBudget::new(crate::storage::BudgetLimits {
            total: 4096,
            block: 4096,
            retained: 0,
        }));
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
pub struct SharedSegmentedVec<T>(Arc<BudgetedSegmentedVec<T>>);
impl<T> SharedSegmentedVec<T> {
    pub fn new(domain: Arc<MemoryBudget>) -> Self {
        Self(Arc::new(BudgetedSegmentedVec::new(
            domain,
            ResourceCategory::Index,
        )))
    }
    pub fn push(&mut self, value: T) -> io::Result<()> {
        Arc::get_mut(&mut self.0)
            .ok_or_else(|| io::Error::other("Shared index is immutable"))?
            .push(value)
    }
    pub fn iter(&self) -> SegmentIter<'_, T> {
        SegmentIter {
            data: &self.0,
            position: 0,
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
        Self(self.0.clone())
    }
}
impl<T> std::ops::Deref for SharedSegmentedVec<T> {
    type Target = BudgetedSegmentedVec<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
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
