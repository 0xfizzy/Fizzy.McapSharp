//! Exactly charged immutable summary declarations; owned record types remain separate.
use crate::{
    charged::{ChargedTree, TreeOwnership},
    segmented::BudgetedSegmentedVec,
    storage::{CopyKind, OwnerKind, Reservation, ResourceCategory},
};
use binrw::BinRead;
use std::{borrow::Cow, collections::BTreeMap, io, ops::Deref, sync::Arc};
fn bytes(value: &[u8], domain: &crate::storage::BudgetRef) -> Result<(Vec<u8>, Reservation),crate::storage::StorageFailure> {
    let (mut data, charge) =
        crate::charged::vector_fixed(domain, ResourceCategory::Declaration, value.len())?;
    data.extend_from_slice(value);
    domain.copy_bytes(CopyKind::Other, value.len());
    Ok((data, charge))
}
fn text(value: &str, domain: &crate::storage::BudgetRef) -> Result<(String, Reservation),crate::storage::StorageFailure> {
    let (data, charge) = bytes(value.as_bytes(), domain)?;
    Ok((String::from_utf8(data).expect("valid UTF-8"), charge))
}
struct SchemaStorage {
    value: crate::Schema<'static>,
    charges: [Reservation; 3],
}
impl TreeOwnership for SchemaStorage {
    fn reference_children(&self, owner: OwnerKind, acquire: bool) {
        for charge in &self.charges {
            charge.owner_reference(owner, acquire);
        }
    }
    fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
}
pub struct SharedSchema(ChargedTree<SchemaStorage>);
impl SharedSchema {
    pub fn new(
        id: u16,
        name: &str,
        encoding: &str,
        data: &[u8],
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let (name, n) = text(name, domain)?;
        let (encoding, e) = text(encoding, domain)?;
        let (data, d) = bytes(data, domain)?;
        Ok(Self(ChargedTree::new_owned_fixed(
            SchemaStorage {
                value: crate::Schema {
                    id,
                    name,
                    encoding,
                    data: Cow::Owned(data),
                },
                charges: [n, e, d],
            },
            domain,
            ResourceCategory::Declaration,
            owner,
        )?))
    }
    /// Explicit ordinary Rust owned export; shared summary storage is unchanged.
    pub fn to_owned_schema(&self) -> Arc<crate::Schema<'static>> {
        Arc::new(self.0.value.clone())
    }
}
impl Clone for SharedSchema {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Deref for SharedSchema {
    type Target = crate::Schema<'static>;
    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}
impl std::fmt::Debug for SharedSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.value.fmt(f)
    }
}
impl PartialEq for SharedSchema {
    fn eq(&self, other: &Self) -> bool {
        self.0.value == other.0.value
    }
}
impl Eq for SharedSchema {}
struct Pair {
    key: String,
    value: String,
    key_charge: Reservation,
    value_charge: Reservation,
}
pub struct Metadata {
    pairs: BudgetedSegmentedVec<Pair>,
}
impl Metadata {
    pub fn len(&self) -> usize {
        self.pairs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
    pub fn iter(&self) -> MetadataIter<'_> {
        MetadataIter {
            metadata: self,
            index: 0,
        }
    }
    pub fn get(&self, key: &str) -> Option<&String> {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let pair = &self.pairs[mid];
            match pair.key.as_str().cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(&pair.value),
            }
        }
        None
    }
    pub fn to_owned_map(&self) -> BTreeMap<String, String> {
        self.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
    fn reference(&self, owner: OwnerKind, acquire: bool) {
        self.pairs.owner_reference(owner, acquire);
        for pair in self.pairs.iter() {
            pair.key_charge.owner_reference(owner, acquire);
            pair.value_charge.owner_reference(owner, acquire);
        }
    }
}
#[derive(Clone)]
pub struct MetadataIter<'a> {
    metadata: &'a Metadata,
    index: usize,
}
impl<'a> Iterator for MetadataIter<'a> {
    type Item = (&'a String, &'a String);
    fn next(&mut self) -> Option<Self::Item> {
        let p = self.metadata.pairs.get(self.index)?;
        self.index += 1;
        Some((&p.key, &p.value))
    }
}
impl std::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl PartialEq for Metadata {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Eq for Metadata {}
pub struct ChannelData {
    pub id: u16,
    pub schema_id: u16,
    pub topic: String,
    pub message_encoding: String,
    pub schema: Option<SharedSchema>,
    pub metadata: Metadata,
}
struct ChannelStorage {
    value: ChannelData,
    charges: [Reservation; 2],
}
impl TreeOwnership for ChannelStorage {
    fn reference_children(&self, owner: OwnerKind, acquire: bool) {
        for charge in &self.charges {
            charge.owner_reference(owner, acquire);
        }
        self.value.metadata.reference(owner, acquire);
    }
    fn mutation_owner(&mut self, _: Option<OwnerKind>) {}
}
pub struct SharedChannel(ChargedTree<ChannelStorage>);
impl SharedChannel {
    /// Metadata must be sorted by unique string keys, as in the official map.
    pub fn new<'a>(
        id: u16,
        topic: &str,
        encoding: &str,
        schema: Option<SharedSchema>,
        metadata: impl Iterator<Item = (&'a str, &'a str)>,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let (topic, t) = text(topic, domain)?;
        let (message_encoding, e) = text(encoding, domain)?;
        let mut pairs =
            BudgetedSegmentedVec::<Pair>::new(domain.clone(), ResourceCategory::Declaration);
        for (key, value) in metadata {
            if pairs.len() > 0 && pairs[pairs.len() - 1].key.as_str() >= key {
                return Err(io::Error::from(io::ErrorKind::InvalidData).into());
            }
            let (key, key_charge) = text(key, domain)?;
            let (value, value_charge) = text(value, domain)?;
            pairs.push_fixed(Pair {
                key,
                value,
                key_charge,
                value_charge,
            })?;
        }
        Ok(Self(ChargedTree::new_owned_fixed(
            ChannelStorage {
                value: ChannelData {
                    id,
                    schema_id: schema.as_ref().map_or(0, |s| s.id),
                    topic,
                    message_encoding,
                    schema,
                    metadata: Metadata { pairs },
                },
                charges: [t, e],
            },
            domain,
            ResourceCategory::Declaration,
            owner,
        )?))
    }
    /// Explicit ordinary Rust owned export, used only by the legacy message helpers.
    pub fn to_owned_channel(&self) -> Arc<crate::Channel<'static>> {
        Arc::new(crate::Channel {
            id: self.id,
            topic: self.topic.clone(),
            message_encoding: self.message_encoding.clone(),
            schema: self.schema.as_ref().map(SharedSchema::to_owned_schema),
            metadata: self.metadata.to_owned_map(),
        })
    }
}
impl Clone for SharedChannel {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Deref for SharedChannel {
    type Target = ChannelData;
    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}
impl std::fmt::Debug for SharedChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedChannel")
            .field("id", &self.id)
            .field("topic", &self.topic)
            .field("schema", &self.schema)
            .field("message_encoding", &self.message_encoding)
            .field("metadata", &self.metadata)
            .finish()
    }
}
impl PartialEq for SharedChannel {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.schema_id == other.schema_id
            && self.topic == other.topic
            && self.message_encoding == other.message_encoding
            && self.schema == other.schema
            && self.metadata == other.metadata
    }
}
impl Eq for SharedChannel {}

pub(crate) fn read_text<'a>(cursor: &mut std::io::Cursor<&'a [u8]>) -> binrw::BinResult<&'a str> {
    let len = u32::read_le(cursor)? as usize;
    let start = cursor.position() as usize;
    let end = start
        .checked_add(len)
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
impl SharedSchema {
    pub fn read(
        data: &[u8],
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let mut cursor = std::io::Cursor::new(data);
        let id = u16::read_le(&mut cursor)?;
        let name = read_text(&mut cursor)?;
        let encoding = read_text(&mut cursor)?;
        let len = u32::read_le(&mut cursor)?;
        let available = &data[cursor.position() as usize..];
        if len > available.len() as u32 {
            return Err(crate::McapError::BadSchemaLength {
                header: len,
                available: available.len() as u32,
            });
        }
        Ok(Self::new(
            id,
            name,
            encoding,
            &available[..len as usize],
            domain,
            owner,
        )?)
    }
}
impl SharedChannel {
    pub fn read(
        data: &[u8],
        schemas: &crate::u16_table::SharedU16Table<SharedSchema>,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        Self::read_with_schema_lookup(data, |id| schemas.get(&id).cloned(), domain, owner)
    }
    /// Parse directly into charged storage using the caller's declaration table.
    pub fn read_with_schema_lookup(
        data: &[u8],
        lookup: impl FnOnce(u16) -> Option<SharedSchema>,
        domain: &crate::storage::BudgetRef,
        owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        Self::read_impl(data, lookup, true, domain, owner)
    }
    /// Retain a wire declaration without requiring its schema to be present.
    pub fn read_record(data: &[u8], domain: &crate::storage::BudgetRef, owner: OwnerKind) -> crate::McapResult<Self> {
        Self::read_impl(data, |_| None, false, domain, owner)
    }
    /// Compare wire fields independently of whether the schema has been resolved.
    pub fn same_declaration(&self, other: &Self) -> bool {
        self.id == other.id && self.schema_id == other.schema_id
            && self.topic == other.topic && self.message_encoding == other.message_encoding
            && self.metadata == other.metadata
    }
    fn read_impl(
        data: &[u8], lookup: impl FnOnce(u16) -> Option<SharedSchema>, require_schema: bool,
        domain: &crate::storage::BudgetRef, owner: OwnerKind,
    ) -> crate::McapResult<Self> {
        let mut cursor = std::io::Cursor::new(data);
        let id = u16::read_le(&mut cursor)?;
        let schema_id = u16::read_le(&mut cursor)?;
        let topic = read_text(&mut cursor)?;
        let encoding = read_text(&mut cursor)?;
        let len = u32::read_le(&mut cursor)?;
        let pos = cursor.position();
        let mut pairs = BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Declaration);
        while cursor.position() - pos < len as u64 {
            let key = read_text(&mut cursor)?;
            let value = read_text(&mut cursor)?;
            let (key, key_charge) = text(key, domain)?;
            let (value, value_charge) = text(value, domain)?;
            pairs.push_fixed(Pair {
                key,
                value,
                key_charge,
                value_charge,
            })?;
        }
        pairs.sort_by(|a, b| a.key.cmp(&b.key));
        for i in 1..pairs.len() {
            if pairs[i - 1].key == pairs[i].key {
                return Err(crate::McapError::StaticParseError {position:pos,description:"Duplicate keys in map"});
            }
        }
        let schema = if schema_id == 0 { None } else { lookup(schema_id) };
        if require_schema && schema_id != 0 && schema.is_none() {
            return Err(crate::McapError::UnknownSchema(topic.into(), schema_id));
        }
        let (topic, t) = text(topic, domain)?;
        let (message_encoding, e) = text(encoding, domain)?;
        Ok(Self(ChargedTree::new_owned_fixed(
            ChannelStorage {
                value: ChannelData {
                    id,
                    schema_id,
                    topic,
                    message_encoding,
                    schema,
                    metadata: Metadata { pairs },
                },
                charges: [t, e],
            },
            domain,
            ResourceCategory::Declaration,
            owner,
        )?))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use binrw::BinWrite;
    fn schema_wire() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        crate::records::SchemaHeader {
            id: 1,
            name: "名字".into(),
            encoding: "raw".into(),
        }
        .write_le(&mut out)
        .unwrap();
        3u32.write_le(&mut out).unwrap();
        std::io::Write::write_all(&mut out, &[1, 2, 3]).unwrap();
        out.into_inner()
    }
    fn channel_wire(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        crate::records::ChannelPairsRef {
            id: 9,
            schema_id: 0,
            topic: "topic",
            message_encoding: "raw",
            metadata: pairs.iter().copied(),
        }
        .write_le(&mut out)
        .unwrap();
        out.into_inner()
    }
    fn compare(data: &[u8], schema: bool) {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let owned = crate::parse_record(
            if schema {
                crate::records::op::SCHEMA
            } else {
                crate::records::op::CHANNEL
            },
            data,
        );
        let shared: crate::McapResult<Vec<u8>> = if schema {
            SharedSchema::read(data, &domain, OwnerKind::Parser).map(|s| {
                let mut out = std::io::Cursor::new(Vec::new());
                crate::records::SummaryRecordRef::Schema(&s)
                    .write_body(&mut out)
                    .unwrap();
                out.into_inner()
            })
        } else {
            let schemas = crate::u16_table::SharedU16Table::new(
                domain.clone(),
                ResourceCategory::Declaration,
            );
            SharedChannel::read(data, &schemas, &domain, OwnerKind::Parser).map(|c| {
                let mut out = std::io::Cursor::new(Vec::new());
                crate::records::SummaryRecordRef::Channel(&c)
                    .write_body(&mut out)
                    .unwrap();
                out.into_inner()
            })
        };
        assert_eq!(owned.is_ok(), shared.is_ok());
        if let (Ok(owned), Ok(wire)) = (owned, shared) {
            match (
                owned,
                crate::parse_record(
                    if schema {
                        crate::records::op::SCHEMA
                    } else {
                        crate::records::op::CHANNEL
                    },
                    &wire,
                )
                .unwrap(),
            ) {
                (
                    crate::records::Record::Schema {
                        header: a,
                        data: ad,
                    },
                    crate::records::Record::Schema {
                        header: b,
                        data: bd,
                    },
                ) => {
                    assert_eq!(a, b);
                    assert_eq!(ad, bd);
                }
                (crate::records::Record::Channel(a), crate::records::Record::Channel(b)) => {
                    assert_eq!(a, b)
                }
                _ => panic!("unexpected declaration"),
            }
        }
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn wire_schema_identity_survives_unresolved_and_resolved_declarations() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut wire = channel_wire(&[("key", "value")]);
        wire[2..4].copy_from_slice(&513u16.to_le_bytes());
        let unresolved = SharedChannel::read_record(&wire, &domain, OwnerKind::Parser).unwrap();
        assert_eq!(unresolved.schema_id, 513);
        assert!(unresolved.schema.is_none());
        let mut output = std::io::Cursor::new(Vec::new());
        crate::records::SummaryRecordRef::Channel(&unresolved).write_body(&mut output).unwrap();
        assert_eq!(output.into_inner(), wire);
        assert!(matches!(SharedChannel::read_with_schema_lookup(&wire, |_| None, &domain, OwnerKind::Parser), Err(crate::McapError::UnknownSchema(_, 513))));
        let schema = SharedSchema::new(513, "schema", "raw", &[7; 64], &domain, OwnerKind::Parser).unwrap();
        let resolved = SharedChannel::read_with_schema_lookup(&wire, |id| { assert_eq!(id, 513); Some(schema.clone()) }, &domain, OwnerKind::Parser).unwrap();
        assert!(unresolved.same_declaration(&resolved));
        wire[2..4].copy_from_slice(&514u16.to_le_bytes());
        let different = SharedChannel::read_record(&wire, &domain, OwnerKind::Parser).unwrap();
        assert!(!unresolved.same_declaration(&different));
        drop((schema, resolved, unresolved, different));
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn direct_parsers_match_owned_lengths_utf8_order_and_duplicates() {
        for (wire, schema) in [
            (schema_wire(), true),
            (
                channel_wire(&[("z", "last"), ("a", "first"), ("中", "文")]),
                false,
            ),
            (channel_wire(&[("a", "one"), ("a", "two")]), false),
        ] {
            for end in 0..=wire.len() {
                compare(&wire[..end], schema);
            }
            for i in 0..wire.len() {
                for value in [0, 0xff] {
                    let mut bad = wire.clone();
                    bad[i] = value;
                    if !schema && (i == 2 || i == 3) {
                        continue;
                    }
                    compare(&bad, schema);
                }
            }
            let mut trailing = wire;
            trailing.extend_from_slice(&[1, 2, 3]);
            compare(&trailing, schema);
        }
    }
    #[test]
    fn shared_declaration_allocation_refusals_release_all_nested_storage() {
        for schema in [true, false] {
            let wire = if schema {
                schema_wire()
            } else {
                channel_wire(&[("a", "1"), ("b", "2"), ("c", "3")])
            };
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            if schema {
                drop(SharedSchema::read(&wire, &domain, OwnerKind::Parser).unwrap());
            } else {
                drop(
                    SharedChannel::read(
                        &wire,
                        &crate::u16_table::SharedU16Table::new(
                            domain.clone(),
                            ResourceCategory::Declaration,
                        ),
                        &domain,
                        OwnerKind::Parser,
                    )
                    .unwrap(),
                );
            }
            let count = domain.workload_detailed_statistics().allocation_count;
            for fail in 0..count {
                let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
                domain.fail_allocation_at(fail as usize);
                if schema {
                    assert!(SharedSchema::read(&wire, &domain, OwnerKind::Parser).is_err());
                } else {
                    assert!(SharedChannel::read(
                        &wire,
                        &crate::u16_table::SharedU16Table::new(
                            domain.clone(),
                            ResourceCategory::Declaration
                        ),
                        &domain,
                        OwnerKind::Parser
                    )
                    .is_err());
                }
                assert_eq!(domain.workload_statistics().current, 0);
                assert_eq!(domain.ownership_statistics(), Default::default());
            }
        }
    }
    #[test]
    fn cross_page_metadata_shares_roots_and_keeps_schema_alive() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let schema = SharedSchema::new(
            1,
            "schema",
            "raw",
            &[42; 4096],
            &domain,
            OwnerKind::Operation,
        )
        .unwrap();
        let fields = (0..2000)
            .map(|i| (format!("key/{i:06}"), format!("value/{i}")))
            .collect::<BTreeMap<_, _>>();
        let channel = SharedChannel::new(
            3,
            "topic",
            "raw",
            Some(schema.clone()),
            fields.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            &domain,
            OwnerKind::Operation,
        )
        .unwrap();
        let count = domain.workload_detailed_statistics().allocation_count;
        let second = channel.clone();
        drop(channel);
        drop(schema);
        assert_eq!(domain.workload_detailed_statistics().allocation_count, count);
        assert_eq!(second.metadata.len(), 2000);
        assert_eq!(second.metadata.get("key/001999").unwrap(), "value/1999");
        assert_eq!(second.schema.as_ref().unwrap().data.as_ref(), &[42; 4096]);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            domain.workload_statistics().current
        );
        drop(second);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
}
