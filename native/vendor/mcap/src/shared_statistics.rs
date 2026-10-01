//! Summary statistics with shared, exactly charged channel-count pages.
//! The ordinary owned record parser remains available through records::Statistics.
use crate::{
    storage::{OwnerKind, ResourceCategory},
    u16_table::SharedU16Table,
};
use binrw::{BinRead, BinResult, BinWrite, Endian};
use std::io::{Read, Seek, Write};

#[derive(Debug, Clone, Default, PartialEq, Eq, BinRead, BinWrite)]
pub struct StatisticsFields {
    pub message_count: u64,
    pub schema_count: u16,
    pub channel_count: u32,
    pub attachment_count: u32,
    pub metadata_count: u32,
    pub chunk_count: u32,
    pub message_start_time: u64,
    pub message_end_time: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedStatistics {
    pub fields: StatisticsFields,
    pub channel_message_counts: SharedU16Table<u64>,
}
impl std::ops::Deref for SharedStatistics {
    type Target = StatisticsFields;
    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}
impl SharedStatistics {
    pub fn read<R: Read + Seek>(
        reader: &mut R,
        domain: crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let fields = StatisticsFields::read_le(reader)?;
        let byte_len = u32::read_le(reader)?;
        let pos = reader.stream_position()?;
        let mut counts = SharedU16Table::new_owned(domain, ResourceCategory::Index, owner);
        // Preserve the official integer-map parser's byte-length semantics,
        // including its treatment of non-multiple lengths and duplicate keys.
        while reader.stream_position()? - pos < byte_len as u64 {
            let key = u16::read_le(reader)?;
            let value = u64::read_le(reader)?;
            if counts.insert_fixed(key, value)?.is_some() {
                return Err(crate::McapError::StaticParseError {position:pos,description:"Duplicate keys in map"});
            }
        }
        Ok(Self {
            fields,
            channel_message_counts: counts,
        })
    }
}
impl BinWrite for SharedStatistics {
    type Args<'a> = ();
    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        endian: Endian,
        (): (),
    ) -> BinResult<()> {
        self.fields.write_options(writer, endian, ())?;
        // At most 65536 u16 keys, so the wire length always fits u32.
        ((self.channel_message_counts.len() * 10) as u32).write_options(writer, endian, ())?;
        for (key, count) in self.channel_message_counts.iter() {
            key.write_options(writer, endian, ())?;
            count.write_options(writer, endian, ())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn fixture() -> Vec<u8> {
        let value = crate::records::Statistics {
            message_count: 99,
            schema_count: 2,
            channel_count: 5,
            attachment_count: 3,
            metadata_count: 4,
            chunk_count: 6,
            message_start_time: 7,
            message_end_time: 88,
            channel_message_counts: [(0, 1), (255, 2), (256, 3), (512, 4), (65535, 89)].into(),
        };
        let mut out = Cursor::new(Vec::new());
        value.write_le(&mut out).unwrap();
        out.into_inner()
    }
    fn compare(bytes: &[u8]) {
        let owned = crate::records::Statistics::read_le(&mut Cursor::new(bytes));
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let shared =
            SharedStatistics::read(&mut Cursor::new(bytes), domain.clone(), OwnerKind::Parser);
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
    fn official_wire_and_corrupt_length_semantics_match() {
        let wire = fixture();
        for end in 0..=wire.len() {
            compare(&wire[..end]);
        }
        for len in [0u32, 1, 9, 10, 11, 49, 50, 51, u32::MAX] {
            let mut mutated = wire.clone();
            mutated[42..46].copy_from_slice(&len.to_le_bytes());
            compare(&mutated);
        }
        let mut duplicate = wire.clone();
        duplicate[56..58].copy_from_slice(&0u16.to_le_bytes());
        compare(&duplicate);
        let mut trailing = wire;
        trailing.extend_from_slice(&[1, 2, 3]);
        compare(&trailing);
    }
    #[test]
    fn parse_allocation_refusals_leave_no_pages_or_owner_pins() {
        let bytes = fixture();
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let parsed =
            SharedStatistics::read(&mut Cursor::new(&bytes), domain.clone(), OwnerKind::Parser)
                .unwrap();
        let calls = domain.workload_detailed_statistics().allocation_count;
        assert_eq!(calls, 5); // shared root plus four populated u16 pages
        drop(parsed);
        for failure in 0..calls {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(SharedStatistics::read(
                &mut Cursor::new(&bytes),
                domain.clone(),
                OwnerKind::Parser
            )
            .is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    #[test]
    fn clone_keeps_pages_alive_without_allocating() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let first = SharedStatistics::read(
            &mut Cursor::new(fixture()),
            domain.clone(),
            OwnerKind::Operation,
        )
        .unwrap();
        let count = domain.workload_detailed_statistics().allocation_count;
        let second = first.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count, count);
        drop(first);
        assert_eq!(second.channel_message_counts[&65535], 89);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            domain.workload_statistics().current
        );
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
