//! Retained declaration keys: exact byte allocations and paged metadata pairs.
//! Borrowed lookup never materializes an owned key.
use crate::{
    segmented::BudgetedSegmentedVec,
    storage::{CopyKind, OwnerKind, Reservation, ResourceCategory},
};
#[cfg(test)]
use std::collections::BTreeMap;
use std::cmp::Ordering;

pub(crate) struct Bytes {
    bytes: Vec<u8>,
    _charge: Reservation,
}
impl Bytes {
    fn new(input: &[u8], domain: &crate::storage::BudgetRef) -> Result<Self,crate::storage::StorageFailure> {
        let (mut bytes, charge) =
            crate::charged::vector_fixed(domain, ResourceCategory::Declaration, input.len())?;
        charge.owner_reference(OwnerKind::Operation, true);
        bytes.extend_from_slice(input);
        domain.copy_bytes(CopyKind::Other, input.len());
        Ok(Self {
            bytes,
            _charge: charge,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("declaration text was copied from str")
    }
}
pub(crate) struct SchemaKey {
    pub name: Bytes,
    pub encoding: Bytes,
    pub data: Bytes,
}
impl SchemaKey {
    pub fn new(
        name: &str,
        encoding: &str,
        data: &[u8],
        domain: &crate::storage::BudgetRef,
    ) -> Result<Self,crate::storage::StorageFailure> {
        Ok(Self {
            name: Bytes::new(name.as_bytes(), domain)?,
            encoding: Bytes::new(encoding.as_bytes(), domain)?,
            data: Bytes::new(data, domain)?,
        })
    }
    pub fn compare(&self, name: &str, encoding: &str, data: &[u8]) -> Ordering {
        (name, encoding, data).cmp(&(self.name.text(), self.encoding.text(), self.data.bytes()))
    }
}
impl Ord for SchemaKey {
    fn cmp(&self, other: &Self) -> Ordering {
        other.compare(self.name.text(), self.encoding.text(), self.data.bytes())
    }
}
impl PartialOrd for SchemaKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for SchemaKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for SchemaKey {}

pub(crate) struct ChannelKey {
    pub topic: Bytes,
    pub schema_id: u16,
    pub message_encoding: Bytes,
    metadata: BudgetedSegmentedVec<(Bytes, Bytes)>,
}
impl ChannelKey {
    #[cfg(test)]
    pub fn new(
        topic: &str,
        schema_id: u16,
        encoding: &str,
        fields: &BTreeMap<String, String>,
        domain: &crate::storage::BudgetRef,
    ) -> Result<Self,crate::storage::StorageFailure> {
        Self::new_pairs(
            topic,
            schema_id,
            encoding,
            fields.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            domain,
        )
    }
    pub fn new_pairs<'a>(
        topic: &str,
        schema_id: u16,
        encoding: &str,
        fields: impl Clone + Iterator<Item = (&'a str, &'a str)>,
        domain: &crate::storage::BudgetRef,
    ) -> Result<Self,crate::storage::StorageFailure> {
        let topic = Bytes::new(topic.as_bytes(), domain)?;
        let message_encoding = Bytes::new(encoding.as_bytes(), domain)?;
        let mut metadata = BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Declaration);
        metadata.reserve_fixed(fields.clone().count())?;
        metadata.owner_reference(OwnerKind::Operation, true);
        for (key, value) in fields {
            metadata.push_fixed((
                Bytes::new(key.as_bytes(), domain)?,
                Bytes::new(value.as_bytes(), domain)?,
            ))?;
        }
        Ok(Self {
            topic,
            schema_id,
            message_encoding,
            metadata,
        })
    }
    pub fn metadata(&self) -> impl Iterator<Item = (&str, &str)> {
        self.metadata
            .iter()
            .map(|(key, value)| (key.text(), value.text()))
    }
    #[cfg(test)]
    pub fn compare(
        &self,
        topic: &str,
        schema_id: u16,
        encoding: &str,
        fields: &BTreeMap<String, String>,
    ) -> Ordering {
        self.compare_pairs(
            topic,
            schema_id,
            encoding,
            fields.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
    }
    pub fn compare_pairs<'a>(
        &self,
        topic: &str,
        schema_id: u16,
        encoding: &str,
        fields: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Ordering {
        (topic, schema_id, encoding)
            .cmp(&(
                self.topic.text(),
                self.schema_id,
                self.message_encoding.text(),
            ))
            .then_with(|| {
                let mut stored = self.metadata();
                for (key, value) in fields {
                    let Some((stored_key, stored_value)) = stored.next() else {
                        return Ordering::Greater;
                    };
                    let order = key.cmp(stored_key).then_with(|| value.cmp(stored_value));
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                if stored.next().is_some() {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
    }
}
impl Ord for ChannelKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.topic.text(),
            self.schema_id,
            self.message_encoding.text(),
        )
            .cmp(&(
                other.topic.text(),
                other.schema_id,
                other.message_encoding.text(),
            ))
            .then_with(|| self.metadata().cmp(other.metadata()))
    }
}
impl PartialOrd for ChannelKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for ChannelKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for ChannelKey {}

#[cfg(test)]
mod tests {
    use super::*;
    fn assert_released(domain: &crate::storage::BudgetRef) {
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn schema_allocations_and_every_refusal_are_exact() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let key = SchemaKey::new("schema/通道", "raw", &[0, 255, 42], &domain).unwrap();
        let expected = "schema/通道".len() as u64 + 6;
        let stats = domain.workload_detailed_statistics();
        assert_eq!(domain.workload_statistics().current, expected);
        assert_eq!(stats.allocation_count, 3);
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            expected
        );
        assert_eq!(
            key.compare("schema/通道", "raw", &[0, 255, 42]),
            Ordering::Equal
        );
        assert_eq!(
            key.compare("schema/通道", "raw", &[0, 255, 43]),
            Ordering::Greater
        );
        drop(key);
        assert_released(&domain);
        for failure in 0..3 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure);
            assert!(SchemaKey::new("name", "encoding", &[1], &domain).is_err());
            assert_released(&domain);
        }
    }
    #[test]
    fn channel_nested_refusal_rolls_back_all_pages_and_strings() {
        let fields: BTreeMap<_, _> = (0..8)
            .map(|n| (format!("key{n}"), format!("value{n}")))
            .collect();
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let key = ChannelKey::new("topic", 42, "encoding", &fields, &domain).unwrap();
        let allocations = domain.workload_detailed_statistics().allocation_count;
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            domain.workload_statistics().current
        );
        drop(key);
        assert_released(&domain);
        for failure in 0..allocations {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(ChannelKey::new("topic", 42, "encoding", &fields, &domain).is_err());
            assert_released(&domain);
        }
    }
    #[test]
    fn channel_metadata_crosses_pages_and_preserves_lexical_lookup() {
        let fields: BTreeMap<_, _> = (0..2000)
            .rev()
            .map(|n| (format!("键{n:04}"), format!("value{n}")))
            .collect();
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let key = ChannelKey::new("topic", 42, "encoding", &fields, &domain).unwrap();
        assert!(key.metadata.allocated_bytes() > 65536);
        assert_eq!(
            key.compare("topic", 42, "encoding", &fields),
            Ordering::Equal
        );
        assert!(key
            .metadata()
            .eq(fields.iter().map(|(k, v)| (k.as_str(), v.as_str()))));
        let mut changed = fields.clone();
        changed.insert("键1000".into(), "changed".into());
        let other = ChannelKey::new("topic", 42, "encoding", &changed, &domain).unwrap();
        assert_eq!(key.cmp(&other), fields.cmp(&changed));
        assert_eq!(
            key.compare("topic", 42, "encoding", &changed),
            changed.cmp(&fields)
        );
        assert_eq!(
            domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
            domain.workload_statistics().current
        );
        drop((key, other));
        assert_released(&domain);
    }
}
