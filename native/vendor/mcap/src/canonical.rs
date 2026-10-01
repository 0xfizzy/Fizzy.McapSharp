//! Canonical declaration lookup with charged u16 pages and AVL content ordering.
//! Node addresses are logical IDs; rotations never move keys or rebuild arrays.
use crate::{
    storage::{OwnerKind, ResourceCategory},
    u16_table::U16Table,
};
use std::{cmp::Ordering, io};
struct Node<K> {
    key: K,
    id: u16,
    left: Option<u16>,
    right: Option<u16>,
    height: u8,
}
pub(crate) struct CanonicalMap<K> {
    nodes: U16Table<Node<K>>,
    root: Option<u16>,
}
impl<K: Ord> CanonicalMap<K> {
    pub fn new(domain: crate::storage::BudgetRef) -> Self {
        Self {
            nodes: U16Table::new_owned(domain, ResourceCategory::Declaration, OwnerKind::Operation),
            root: None,
        }
    }
    pub fn get_by_left(&self, key: &K) -> Option<&u16> {
        self.find_by(|stored| key.cmp(stored))
    }
    pub fn find_by(&self, compare: impl Fn(&K) -> Ordering) -> Option<&u16> {
        let mut current = self.root;
        while let Some(id) = current {
            let n = self.nodes.get(&id).unwrap();
            match compare(&n.key) {
                Ordering::Less => current = n.left,
                Ordering::Greater => current = n.right,
                Ordering::Equal => return Some(&n.id),
            }
        }
        None
    }
    pub fn get_by_right(&self, id: &u16) -> Option<&K> {
        self.nodes.get(id).map(|n| &n.key)
    }
    pub fn insert_no_overwrite(&mut self, key: K, id: u16) -> crate::McapResult<()> {
        if self.nodes.contains_key(&id) || self.get_by_left(&key).is_some() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
        }
        // Only this insertion allocates. Refusal leaves both search directions intact.
        self.nodes.insert_fixed(
            id,
            Node {
                key,
                id,
                left: None,
                right: None,
                height: 1,
            },
        )?;
        self.root = Some(self.link(self.root, id));
        Ok(())
    }
    fn height(&self, id: Option<u16>) -> u8 {
        id.map_or(0, |id| self.nodes.get(&id).unwrap().height)
    }
    fn update(&mut self, id: u16) {
        let n = self.nodes.get(&id).unwrap();
        let height = 1 + self.height(n.left).max(self.height(n.right));
        self.nodes.get_mut(&id).unwrap().height = height;
    }
    fn left(&mut self, id: u16) -> u16 {
        let right = self.nodes.get(&id).unwrap().right.unwrap();
        let middle = self.nodes.get(&right).unwrap().left;
        self.nodes.get_mut(&id).unwrap().right = middle;
        self.nodes.get_mut(&right).unwrap().left = Some(id);
        self.update(id);
        self.update(right);
        right
    }
    fn right(&mut self, id: u16) -> u16 {
        let left = self.nodes.get(&id).unwrap().left.unwrap();
        let middle = self.nodes.get(&left).unwrap().right;
        self.nodes.get_mut(&id).unwrap().left = middle;
        self.nodes.get_mut(&left).unwrap().right = Some(id);
        self.update(id);
        self.update(left);
        left
    }
    fn link(&mut self, root: Option<u16>, id: u16) -> u16 {
        let Some(root) = root else { return id };
        if self.nodes.get(&id).unwrap().key < self.nodes.get(&root).unwrap().key {
            let child = self.link(self.nodes.get(&root).unwrap().left, id);
            self.nodes.get_mut(&root).unwrap().left = Some(child);
        } else {
            let child = self.link(self.nodes.get(&root).unwrap().right, id);
            self.nodes.get_mut(&root).unwrap().right = Some(child);
        }
        self.update(root);
        let n = self.nodes.get(&root).unwrap();
        let (left, right) = (n.left, n.right);
        let balance = self.height(left) as i16 - self.height(right) as i16;
        if balance > 1 {
            let l = left.unwrap();
            let n = self.nodes.get(&l).unwrap();
            if self.height(n.left) < self.height(n.right) {
                let rotated = self.left(l);
                self.nodes.get_mut(&root).unwrap().left = Some(rotated);
            }
            return self.right(root);
        }
        if balance < -1 {
            let r = right.unwrap();
            let n = self.nodes.get(&r).unwrap();
            if self.height(n.right) < self.height(n.left) {
                let rotated = self.right(r);
                self.nodes.get_mut(&root).unwrap().right = Some(rotated);
            }
            return self.left(root);
        }
        root
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_ids_bidirectional_lookup_balance_and_unique_owner_capacity() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut map = CanonicalMap::new(domain.clone());
        for id in 0..=u16::MAX {
            map.insert_no_overwrite(u16::MAX - id, id).unwrap();
        }
        for id in 0..=u16::MAX {
            assert_eq!(map.get_by_left(&(u16::MAX - id)), Some(&id));
            assert_eq!(map.get_by_right(&id), Some(&(u16::MAX - id)));
        }
        assert!(map.height(map.root) < 24);
        let current = domain.workload_statistics().current;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            current
        );
        assert!(map.insert_no_overwrite(0, 0).is_err());
        assert_eq!(domain.workload_statistics().current, current);
        drop(map);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn refused_new_page_preserves_both_indexes_and_existing_page_accepts_more() {
        let probe = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut map = CanonicalMap::new(probe.clone());
        map.insert_no_overwrite(1u64, 0).unwrap();
        let page = probe.workload_statistics().current as usize;
        drop(map);
        let domain = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            total: (page) + crate::storage::BudgetRef::allocation_size(),
            block: page,
            retained: 0,
        }).unwrap();
        let mut map = CanonicalMap::new(domain.clone());
        map.insert_no_overwrite(1u64, 0).unwrap();
        assert!(map.insert_no_overwrite(2, 256).is_err());
        assert_eq!(map.get_by_left(&1), Some(&0));
        assert_eq!(map.get_by_right(&0), Some(&1));
        assert_eq!(map.get_by_left(&2), None);
        assert_eq!(map.get_by_right(&256), None);
        map.insert_no_overwrite(2, 1).unwrap();
        assert_eq!(map.get_by_right(&1), Some(&2));
        drop(map);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
}
