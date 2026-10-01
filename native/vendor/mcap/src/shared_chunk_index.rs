//! Chunk indexes with exactly charged compression text and paged channel offsets.
use crate::{
    charged::{ChargedTree, TreeOwnership},
    storage::{CopyKind, OwnerKind, Reservation, ResourceCategory},
    u16_table::U16Table,
};
use binrw::{BinRead, BinResult, BinWrite, Endian};
use std::{
    io::{self, Cursor, Seek, Write},
    ops::Deref,
};

#[derive(Debug, Clone, PartialEq, Eq, BinRead, BinWrite)]
pub struct ChunkIndexFields {
    pub message_start_time: u64,
    pub message_end_time: u64,
    pub chunk_start_offset: u64,
    pub chunk_length: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct ChunkIndexData {
    pub fields: ChunkIndexFields,
    pub message_index_length: u64,
    pub compression: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}
impl Deref for ChunkIndexData {
    type Target = ChunkIndexFields;
    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}
struct Storage {
    data: ChunkIndexData,
    compression_charge: Reservation,
}
impl TreeOwnership for Storage {
    fn reference_children(&self, owner: OwnerKind, acquire: bool) {
        self.compression_charge.owner_reference(owner, acquire);
    }
    fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
}
// Most chunks contain only one or two channels. Keep those offsets in the
// already charged descriptor, allocating a u16 directory only when necessary.
#[derive(Clone)]
pub enum ChunkOffsets {
    Inline {
        entries: [(u16, u64); 4],
        len: usize,
    },
    Paged(ChargedTree<U16Table<u64>>),
}
impl ChunkOffsets {
    pub(crate) fn empty() -> Self {
        Self::Inline {
            entries: [(0, 0); 4],
            len: 0,
        }
    }
    pub(crate) fn insert(
        &mut self,
        key: u16,
        value: u64,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Option<u64>> {
        match self {
            Self::Paged(root) => Ok(root
                .get_mut()
                .ok_or(crate::McapError::StaticIoError("shared offsets are immutable"))?
                .insert_fixed(key, value)?),
            Self::Inline { entries, len } => {
                if let Some(entry) = entries[..*len].iter_mut().find(|entry| entry.0 == key) {
                    return Ok(Some(std::mem::replace(&mut entry.1, value)));
                }
                if *len < entries.len() {
                    entries[*len] = (key, value);
                    *len += 1;
                    entries[..*len].sort_unstable_by_key(|entry| entry.0);
                    return Ok(None);
                }
                let mut root = ChargedTree::new_owned_fixed(
                    U16Table::new(domain.clone(), ResourceCategory::Index),
                    domain,
                    ResourceCategory::Index,
                    owner,
                )?;
                for &(key, value) in entries.iter() {
                    root.get_mut().unwrap().insert_fixed(key, value)?;
                }
                root.get_mut().unwrap().insert_fixed(key, value)?;
                *self = Self::Paged(root);
                Ok(None)
            }
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Self::Inline { len, .. } => *len,
            Self::Paged(root) => root.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn keys(&self) -> impl Iterator<Item = u16> + '_ {
        self.iter().map(|(key, _)| key)
    }
    pub fn values(&self) -> impl Iterator<Item = &u64> {
        self.iter().map(|(_, value)| value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u16, &u64)> {
        let inline = match self {
            Self::Inline { entries, len } => &entries[..*len],
            _ => &[],
        };
        let paged = match self {
            Self::Paged(root) => Some(root),
            _ => None,
        };
        inline
            .iter()
            .map(|(key, value)| (*key, value))
            .chain(paged.into_iter().flat_map(|root| root.iter()))
    }
}
impl std::ops::Index<&u16> for ChunkOffsets {
    type Output = u64;
    fn index(&self, key: &u16) -> &u64 {
        self.iter()
            .find(|(id, _)| id == key)
            .expect("missing channel offset")
            .1
    }
}
impl std::fmt::Debug for ChunkOffsets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl PartialEq for ChunkOffsets {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Eq for ChunkOffsets {}
pub struct SharedChunkIndex {
    storage: ChargedTree<Storage>,
    pub message_index_offsets: ChunkOffsets,
}
impl SharedChunkIndex {
    pub fn read(data: &[u8], domain: &crate::storage::BudgetRef, owner: OwnerKind) -> crate::McapResult<Self> {
        let mut cursor = Cursor::new(data);
        let fields = ChunkIndexFields::read_le(&mut cursor)?;
        let byte_len = u32::read_le(&mut cursor)?;
        let pos = cursor.position();
        let mut offsets = ChunkOffsets::empty();
        // Match the official integer-map parser, including non-multiple lengths.
        while cursor.position() - pos < byte_len as u64 {
            let key = u16::read_le(&mut cursor)?;
            let value = u64::read_le(&mut cursor)?;
            if offsets.insert(key, value, domain, owner)?.is_some() {
                return Err(crate::McapError::StaticParseError {position:pos,description:"Duplicate keys in map"});
            }
        }
        let message_index_length = u64::read_le(&mut cursor)?;
        let len = u32::read_le(&mut cursor)? as usize;
        let start = cursor.position() as usize;
        let end = start
            .checked_add(len)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let bytes = data
            .get(start..end)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let compression =
            std::str::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        cursor.set_position(end as u64);
        let compressed_size = u64::read_le(&mut cursor)?;
        let uncompressed_size = u64::read_le(&mut cursor)?;
        Ok(Self::new(
            fields,
            message_index_length,
            compression,
            compressed_size,
            uncompressed_size,
            offsets,
            domain,
            owner,
        )?)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fields: ChunkIndexFields,
        message_index_length: u64,
        compression: &str,
        compressed_size: u64,
        uncompressed_size: u64,
        offsets: ChunkOffsets,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let (mut bytes, compression_charge) =
            crate::charged::vector_fixed(domain, ResourceCategory::Index, compression.len())?;
        bytes.extend_from_slice(compression.as_bytes());
        domain.copy_bytes(CopyKind::Other, compression.len());
        let storage = ChargedTree::new_owned_fixed(
            Storage {
                data: ChunkIndexData {
                    fields,
                    message_index_length,
                    compression: String::from_utf8(bytes).expect("validated UTF-8"),
                    compressed_size,
                    uncompressed_size,
                },
                compression_charge,
            },
            domain,
            ResourceCategory::Index,
            owner,
        )?;
        Ok(Self {
            storage,
            message_index_offsets: offsets,
        })
    }
    pub fn compressed_data_offset(&self) -> crate::McapResult<u64> {
        compressed_data_offset(self.chunk_start_offset, self.compression.len())
    }
}
impl Deref for SharedChunkIndex {
    type Target = ChunkIndexData;
    fn deref(&self) -> &Self::Target {
        &self.storage.data
    }
}
impl Clone for SharedChunkIndex {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            message_index_offsets: self.message_index_offsets.clone(),
        }
    }
}
impl std::fmt::Debug for SharedChunkIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedChunkIndex")
            .field("data", &self.storage.data)
            .field("message_index_offsets", &self.message_index_offsets)
            .finish()
    }
}
impl PartialEq for SharedChunkIndex {
    fn eq(&self, other: &Self) -> bool {
        self.storage.data == other.storage.data
            && self.message_index_offsets == other.message_index_offsets
    }
}
impl Eq for SharedChunkIndex {}
impl BinWrite for SharedChunkIndex {
    type Args<'a> = ();
    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        endian: Endian,
        (): (),
    ) -> BinResult<()> {
        self.fields.write_options(writer, endian, ())?;
        ((self.message_index_offsets.len() * 10) as u32).write_options(writer, endian, ())?;
        for (key, offset) in self.message_index_offsets.iter() {
            key.write_options(writer, endian, ())?;
            offset.write_options(writer, endian, ())?;
        }
        self.message_index_length
            .write_options(writer, endian, ())?;
        (self.compression.len() as u32).write_options(writer, endian, ())?;
        writer.write_all(self.compression.as_bytes())?;
        self.compressed_size.write_options(writer, endian, ())?;
        self.uncompressed_size.write_options(writer, endian, ())?;
        Ok(())
    }
}

/// Borrowed helper input; both ordinary owned and charged indexes implement it.
pub trait ChunkIndexAccess {
    fn index_view(&self) -> ChunkIndexView<'_>;
}
pub struct ChunkIndexView<'a> {
    pub message_start_time: u64,
    pub message_end_time: u64,
    pub chunk_start_offset: u64,
    pub chunk_length: u64,
    pub message_index_length: u64,
    pub compression: &'a str,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub message_index_offsets: OffsetView<'a>,
}
pub enum OffsetView<'a> {
    Owned(&'a std::collections::BTreeMap<u16, u64>),
    Shared(&'a ChunkOffsets),
}
impl OffsetView<'_> {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Owned(map) => map.is_empty(),
            Self::Shared(map) => map.is_empty(),
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (u16, &u64)> {
        let owned = match self {
            Self::Owned(map) => Some(*map),
            _ => None,
        };
        let shared = match self {
            Self::Shared(map) => Some(*map),
            _ => None,
        };
        owned
            .into_iter()
            .flat_map(|map| map.iter().map(|(key, value)| (*key, value)))
            .chain(shared.into_iter().flat_map(|map| map.iter()))
    }
}
macro_rules! index_view {
    ($index:expr, $offsets:expr) => {
        ChunkIndexView {
            message_start_time: $index.message_start_time,
            message_end_time: $index.message_end_time,
            chunk_start_offset: $index.chunk_start_offset,
            chunk_length: $index.chunk_length,
            message_index_length: $index.message_index_length,
            compression: &$index.compression,
            compressed_size: $index.compressed_size,
            uncompressed_size: $index.uncompressed_size,
            message_index_offsets: $offsets,
        }
    };
}
impl ChunkIndexAccess for SharedChunkIndex {
    fn index_view(&self) -> ChunkIndexView<'_> {
        index_view!(self, OffsetView::Shared(&self.message_index_offsets))
    }
}
impl ChunkIndexAccess for crate::records::ChunkIndex {
    fn index_view(&self) -> ChunkIndexView<'_> {
        index_view!(self, OffsetView::Owned(&self.message_index_offsets))
    }
}
pub fn compressed_data_offset(offset: u64, compression_len: usize) -> crate::McapResult<u64> {
    u64::try_from(compression_len)
        .ok()
        .and_then(|n| n.checked_add(49))
        .and_then(|n| offset.checked_add(n))
        .ok_or(crate::McapError::BadChunkStartOffset(offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire() -> Vec<u8> {
        let index = crate::records::ChunkIndex {
            message_start_time: 1,
            message_end_time: 2,
            chunk_start_offset: 3,
            chunk_length: 4,
            message_index_offsets: [(0, 12), (255, 13), (256, 14), (512, 16), (65535, 15)].into(),
            message_index_length: 16,
            compression: "压缩🌍".into(),
            compressed_size: 17,
            uncompressed_size: 18,
        };
        let mut output = Cursor::new(Vec::new());
        index.write_le(&mut output).unwrap();
        output.into_inner()
    }
    fn compare(bytes: &[u8]) {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let owned = crate::records::ChunkIndex::read_le(&mut Cursor::new(bytes));
        let shared = SharedChunkIndex::read(bytes, &domain, OwnerKind::Parser);
        assert_eq!(owned.is_ok(), shared.is_ok());
        if let (Ok(owned), Ok(shared)) = (owned, shared) {
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
    fn official_wire_lengths_duplicates_and_utf8_match() {
        let bytes = wire();
        for end in 0..=bytes.len() {
            compare(&bytes[..end]);
        }
        for len in [0u32, 1, 9, 10, 11, 39, 40, 41, 49, 50, 51, u32::MAX] {
            let mut changed = bytes.clone();
            changed[32..36].copy_from_slice(&len.to_le_bytes());
            compare(&changed);
        }
        let mut duplicate = bytes.clone();
        duplicate[46..48].copy_from_slice(&0u16.to_le_bytes());
        compare(&duplicate);
        let mut invalid = bytes.clone();
        invalid[98] = 0xff;
        compare(&invalid);
        for len in [0u32, 1, 2, 100, u32::MAX] {
            let mut changed = bytes.clone();
            changed[94..98].copy_from_slice(&len.to_le_bytes());
            compare(&changed);
        }
        let mut trailing = bytes;
        trailing.extend_from_slice(&[1, 2, 3]);
        compare(&trailing);
    }
    #[test]
    fn inline_offsets_and_failed_promotion_preserve_existing_values() {
        for failure in 0..5 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut offsets = ChunkOffsets::empty();
            for key in [65535, 256, 255, 0] {
                offsets
                    .insert(key, key as u64, &domain, OwnerKind::Parser)
                    .unwrap();
            }
            assert_eq!(domain.workload_detailed_statistics().allocation_count, 0);
            domain.fail_allocation_at(failure);
            assert!(offsets.insert(512, 19, &domain, OwnerKind::Parser).is_err());
            assert_eq!(offsets.len(), 4);
            assert_eq!(offsets[&65535], 65535);
            assert_eq!(
                offsets.iter().map(|(key, _)| key).collect::<Vec<_>>(),
                [0, 255, 256, 65535]
            );
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    #[test]
    fn clone_shares_exact_storage_and_every_refusal_rolls_back() {
        let bytes = wire();
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let first = SharedChunkIndex::read(&bytes, &domain, OwnerKind::Parser).unwrap();
        let calls = domain.workload_detailed_statistics().allocation_count;
        assert_eq!(calls, 7); // u16 root, four pages, compression bytes, shared control
        let second = first.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, calls);
        drop(first);
        assert_eq!(second.message_index_offsets[&65535], 15);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
            domain.workload_statistics().current
        );
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
        for failure in 0..calls {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(SharedChunkIndex::read(&bytes, &domain, OwnerKind::Parser).is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
}
