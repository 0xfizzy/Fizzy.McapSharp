//! Immutable metadata index with exact name and control allocations.
use crate::{
    charged::{ChargedTree, TreeOwnership},
    records::MetadataIndex,
    storage::{CopyKind, OwnerKind, Reservation, ResourceCategory},
};
use binrw::{BinRead, BinResult, BinWrite, Endian};
use std::{
    io::{self, Cursor, Seek, Write},
    ops::Deref,
};

struct Storage {
    record: MetadataIndex,
    name_charge: Reservation,
}
impl TreeOwnership for Storage {
    fn reference_children(&self, owner: OwnerKind, acquire: bool) {
        self.name_charge.owner_reference(owner, acquire);
    }
    fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
}
/// The owned record view borrows this owner's exactly charged storage.
/// Cloning shares both the record and its name; no String is cloned.
pub struct SharedMetadataIndex(ChargedTree<Storage>);
impl SharedMetadataIndex {
    pub fn new(
        offset: u64,
        length: u64,
        name: &str,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let (mut bytes, charge) =
            crate::charged::vector_fixed(domain, ResourceCategory::Index, name.len())?;
        bytes.extend_from_slice(name.as_bytes());
        domain.copy_bytes(CopyKind::Other, name.len());
        // Input is already UTF-8, and conversion preserves the exact allocation.
        let name = String::from_utf8(bytes).expect("copied valid UTF-8");
        let storage = Storage {
            record: MetadataIndex {
                offset,
                length,
                name,
            },
            name_charge: charge,
        };
        Ok(Self(ChargedTree::new_owned_fixed(
            storage,
            domain,
            ResourceCategory::Index,
            owner,
        )?))
    }
    pub fn read(data: &[u8], domain: &crate::storage::BudgetRef, owner: OwnerKind) -> crate::McapResult<Self> {
        let mut cursor = Cursor::new(data);
        let offset = u64::read_le(&mut cursor)?;
        let length = u64::read_le(&mut cursor)?;
        let size = u32::read_le(&mut cursor)? as usize;
        let start = cursor.position() as usize;
        let end = start
            .checked_add(size)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let bytes = data
            .get(start..end)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let name =
            std::str::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        Ok(Self::new(offset, length, name, domain, owner)?)
    }
}
impl Clone for SharedMetadataIndex {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Deref for SharedMetadataIndex {
    type Target = MetadataIndex;
    fn deref(&self) -> &Self::Target {
        &self.0.record
    }
}
impl std::fmt::Debug for SharedMetadataIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.record.fmt(f)
    }
}
impl PartialEq for SharedMetadataIndex {
    fn eq(&self, other: &Self) -> bool {
        self.0.record == other.0.record
    }
}
impl Eq for SharedMetadataIndex {}
impl BinWrite for SharedMetadataIndex {
    type Args<'a> = ();
    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        endian: Endian,
        (): (),
    ) -> BinResult<()> {
        self.0.record.write_options(writer, endian, ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire(name: &str) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        MetadataIndex {
            offset: 42,
            length: 73,
            name: name.into(),
        }
        .write_le(&mut out)
        .unwrap();
        out.into_inner()
    }
    fn compare(bytes: &[u8]) {
        let owned = MetadataIndex::read_le(&mut Cursor::new(bytes));
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let shared = SharedMetadataIndex::read(bytes, &domain, OwnerKind::Parser);
        assert_eq!(owned.is_ok(), shared.is_ok());
        if let (Ok(owned), Ok(shared)) = (owned, shared) {
            assert_eq!(*shared, owned);
            let mut original = Cursor::new(Vec::new());
            let mut actual = Cursor::new(Vec::new());
            owned.write_le(&mut original).unwrap();
            shared.write_le(&mut actual).unwrap();
            assert_eq!(original.into_inner(), actual.into_inner());
        }
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn borrowed_parser_matches_owned_record_for_names_lengths_and_truncation() {
        for name in ["", "metadata", "名字🌍\0tail"] {
            let bytes = wire(name);
            for end in 0..=bytes.len() {
                compare(&bytes[..end]);
            }
            for len in [0u32, 1, 2, 100, u32::MAX] {
                let mut changed = bytes.clone();
                changed[16..20].copy_from_slice(&len.to_le_bytes());
                compare(&changed);
            }
            let mut trailing = bytes;
            trailing.extend_from_slice(&[1, 2, 3]);
            compare(&trailing);
        }
        let mut invalid = wire("abc");
        invalid[20] = 0xff;
        compare(&invalid);
    }
    #[test]
    fn exact_names_shared_owners_and_refusal_cleanup() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let first = SharedMetadataIndex::new(1, 2, "名字", &domain, OwnerKind::Operation).unwrap();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 2);
        let bytes = domain.workload_statistics().current;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            bytes
        );
        let second = first.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 2);
        drop(first);
        assert_eq!(second.name, "名字");
        assert_eq!(domain.workload_statistics().current, bytes);
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        for failure in 0..2 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure);
            assert!(SharedMetadataIndex::new(1, 2, "name", &domain, OwnerKind::Operation).is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    #[test]
    fn writer_index_allocation_failures_are_terminal_after_advancement() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer = crate::WriteOptions::new()
            .use_chunks(false)
            .memory_budget(domain.clone())
            .create(Cursor::new(Vec::new()))
            .unwrap();
        let before = domain.workload_detailed_statistics().allocation_count;
        writer
            .write_metadata_borrowed("name", std::iter::empty())
            .unwrap();
        let allocations = domain.workload_detailed_statistics().allocation_count - before;
        assert!(allocations >= 2);
        drop(writer.into_inner());
        assert_eq!(domain.workload_statistics().current, 0);
        for failure in 0..allocations {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut writer = crate::WriteOptions::new()
                .use_chunks(false)
                .memory_budget(domain.clone())
                .create(Cursor::new(Vec::new()))
                .unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(writer
                .write_metadata_borrowed("name", std::iter::empty())
                .is_err());
            assert!(matches!(
                writer.finish(),
                Err(crate::McapError::AttemptedWriteAfterFailure)
            ));
            drop(writer.into_inner());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
}
