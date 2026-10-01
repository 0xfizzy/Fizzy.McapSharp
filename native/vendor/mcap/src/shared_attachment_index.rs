//! Stable attachment-index storage shared from header emission through summary disposal.
use crate::{
    charged::{ChargedTree, TreeOwnership},
    records::AttachmentIndex,
    storage::{CopyKind, OwnerKind, Reservation, ResourceCategory},
};
use binrw::{BinRead, BinResult, BinWrite, Endian};
use std::{
    io::{self, Cursor, Seek, Write},
    ops::Deref,
};
struct Storage {
    record: AttachmentIndex,
    name_charge: Reservation,
    media_charge: Reservation,
}
impl TreeOwnership for Storage {
    fn reference_children(&self, owner: OwnerKind, acquire: bool) {
        self.name_charge.owner_reference(owner, acquire);
        self.media_charge.owner_reference(owner, acquire);
    }
    fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
}
pub struct SharedAttachmentIndex(ChargedTree<Storage>);
fn text(value: &str, domain: &crate::storage::BudgetRef) -> Result<(String, Reservation),crate::storage::StorageFailure> {
    let (mut bytes, charge) = crate::charged::vector_fixed(domain, ResourceCategory::Index, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    domain.copy_bytes(CopyKind::Other, value.len());
    Ok((
        String::from_utf8(bytes).expect("copied valid UTF-8"),
        charge,
    ))
}
fn read_text<'a>(cursor: &mut Cursor<&'a [u8]>) -> BinResult<&'a str> {
    let size = u32::read_le(cursor)? as usize;
    let start = cursor.position() as usize;
    let end = start
        .checked_add(size)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    let bytes = cursor
        .get_ref()
        .get(start..end)
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    let value =
        std::str::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    cursor.set_position(end as u64);
    Ok(value)
}
impl SharedAttachmentIndex {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        offset: u64,
        length: u64,
        log_time: u64,
        create_time: u64,
        data_size: u64,
        name: &str,
        media_type: &str,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let (name, name_charge) = text(name, domain)?;
        let (media_type, media_charge) = text(media_type, domain)?;
        let storage = Storage {
            record: AttachmentIndex {
                offset,
                length,
                log_time,
                create_time,
                data_size,
                name,
                media_type,
            },
            name_charge,
            media_charge,
        };
        Ok(Self(ChargedTree::new_owned_fixed(
            storage,
            domain,
            ResourceCategory::Index,
            owner,
        )?))
    }
    /// Only the unpublished writer index may be updated. No name or control is copied.
    pub(crate) fn finish_length(&mut self, length: u64) {
        self.0
            .get_mut()
            .expect("unpublished attachment index must be unique")
            .record
            .length = length;
    }
    pub fn read(data: &[u8], domain: &crate::storage::BudgetRef, owner: OwnerKind) -> crate::McapResult<Self> {
        let mut cursor = Cursor::new(data);
        let offset = u64::read_le(&mut cursor)?;
        let length = u64::read_le(&mut cursor)?;
        let log_time = u64::read_le(&mut cursor)?;
        let create_time = u64::read_le(&mut cursor)?;
        let data_size = u64::read_le(&mut cursor)?;
        let name = read_text(&mut cursor)?;
        let media_type = read_text(&mut cursor)?;
        Ok(Self::new(
            offset,
            length,
            log_time,
            create_time,
            data_size,
            name,
            media_type,
            domain,
            owner,
        )?)
    }
}
impl Clone for SharedAttachmentIndex {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Deref for SharedAttachmentIndex {
    type Target = AttachmentIndex;
    fn deref(&self) -> &Self::Target {
        &self.0.record
    }
}
impl std::fmt::Debug for SharedAttachmentIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.record.fmt(f)
    }
}
impl PartialEq for SharedAttachmentIndex {
    fn eq(&self, other: &Self) -> bool {
        self.0.record == other.0.record
    }
}
impl Eq for SharedAttachmentIndex {}
impl BinWrite for SharedAttachmentIndex {
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
    fn wire(name: &str, media: &str) -> Vec<u8> {
        let record = AttachmentIndex {
            offset: 1,
            length: 2,
            log_time: 3,
            create_time: 4,
            data_size: 5,
            name: name.into(),
            media_type: media.into(),
        };
        let mut out = Cursor::new(Vec::new());
        record.write_le(&mut out).unwrap();
        out.into_inner()
    }
    fn compare(bytes: &[u8]) {
        let owned = AttachmentIndex::read_le(&mut Cursor::new(bytes));
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let shared = SharedAttachmentIndex::read(bytes, &domain, OwnerKind::Parser);
        assert_eq!(owned.is_ok(), shared.is_ok());
        if let (Ok(owned), Ok(shared)) = (owned, shared) {
            assert_eq!(*shared, owned);
            let mut a = Cursor::new(Vec::new());
            let mut b = Cursor::new(Vec::new());
            owned.write_le(&mut a).unwrap();
            shared.write_le(&mut b).unwrap();
            assert_eq!(a.into_inner(), b.into_inner());
        }
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn official_wire_and_malformed_strings_match() {
        for (name, media) in [("", ""), ("file", "raw"), ("名字🌍\0", "类型/数据")] {
            let bytes = wire(name, media);
            for end in 0..=bytes.len() {
                compare(&bytes[..end]);
            }
            for position in [40, 44 + name.len()] {
                for length in [0u32, 1, 100, u32::MAX] {
                    let mut changed = bytes.clone();
                    changed[position..position + 4].copy_from_slice(&length.to_le_bytes());
                    compare(&changed);
                }
            }
            let mut trailing = bytes;
            trailing.extend_from_slice(&[1, 2, 3]);
            compare(&trailing);
        }
        for position in [44, 52] {
            let mut bytes = wire("file", "raw");
            bytes[position] = 255;
            compare(&bytes);
        }
    }
    #[test]
    fn names_and_control_share_exact_ownership_and_rollback() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let index =
            SharedAttachmentIndex::new(1, 2, 3, 4, 5, "name", "raw", &domain, OwnerKind::Operation)
                .unwrap();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 3);
        let bytes = domain.workload_statistics().current;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            bytes
        );
        let second = index.clone();
        drop(index);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 3);
        assert_eq!(second.name, "name");
        assert_eq!(second.media_type, "raw");
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
        for failure in 0..3 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure);
            assert!(SharedAttachmentIndex::new(
                1,
                2,
                3,
                4,
                5,
                "name",
                "raw",
                &domain,
                OwnerKind::Operation
            )
            .is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    fn writer(domain: crate::storage::BudgetRef) -> crate::Writer<Cursor<Vec<u8>>> {
        crate::WriteOptions::new()
            .use_chunks(false)
            .memory_budget(domain)
            .create(Cursor::new(Vec::new()))
            .unwrap()
    }
    #[test]
    fn header_names_are_final_index_storage_without_finish_copy() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut w = writer(domain.clone());
        let before = domain.workload_detailed_statistics();
        w.start_attachment_borrowed(3, 1, 2, "name", "raw").unwrap();
        let started = domain.workload_detailed_statistics();
        assert_eq!(started.allocation_count - before.allocation_count, 3);
        assert_eq!(started.flow.other_copy - before.flow.other_copy, 7);
        w.put_attachment_bytes(&[1, 2, 3]).unwrap();
        w.finish_attachment().unwrap();
        let summary = w.finish().unwrap();
        assert_eq!(
            domain.workload_detailed_statistics().flow.other_copy,
            started.flow.other_copy
        );
        let copy = summary.clone();
        drop(summary);
        drop(w.into_inner());
        assert_eq!(copy.attachment_indexes[0].name, "name");
        assert_eq!(copy.attachment_indexes[0].media_type, "raw");
        assert_eq!(copy.attachment_indexes[0].data_size, 3);
        drop(copy);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn rejected_header_and_index_publication_leave_a_terminal_writer() {
        for failure in 0..3 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut w = writer(domain.clone());
            domain.fail_allocation_at(failure);
            assert!(w.start_attachment_borrowed(3, 1, 2, "name", "raw").is_err());
            assert!(matches!(
                w.finish(),
                Err(crate::McapError::AttemptedWriteAfterFailure)
            ));
            drop(w.into_inner());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut w = writer(domain.clone());
        w.start_attachment_borrowed(3, 1, 2, "name", "raw").unwrap();
        w.put_attachment_bytes(&[1, 2, 3]).unwrap();
        let before = domain.workload_detailed_statistics().allocation_count;
        w.finish_attachment().unwrap();
        let calls = domain.workload_detailed_statistics().allocation_count - before;
        drop(w.into_inner());
        assert!(calls > 0);
        for failure in 0..calls {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut w = writer(domain.clone());
            w.start_attachment_borrowed(3, 1, 2, "name", "raw").unwrap();
            w.put_attachment_bytes(&[1, 2, 3]).unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(w.finish_attachment().is_err());
            assert!(matches!(
                w.finish(),
                Err(crate::McapError::AttemptedWriteAfterFailure)
            ));
            drop(w.into_inner());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
}
