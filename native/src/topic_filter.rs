//! Retained reader filters: exact string capacity and paged descriptors.
use super::*;
use mcap::storage::{BudgetRef, Reservation, ResourceCategory, OwnerKind, CopyKind};
pub(super) struct Text { value: String, charge: Reservation }
impl Text {
    pub fn new(value: &str, domain: &BudgetRef) -> Outcome<Self> {
        let (mut bytes, charge) = mcap::charged::vector_fixed(domain, ResourceCategory::Declaration, value.len())?;
        bytes.extend_from_slice(value.as_bytes());
        domain.copy_bytes(CopyKind::Other, value.len());
        charge.owner_reference(OwnerKind::Parser, true);
        Ok(Self { value: String::from_utf8(bytes).expect("validated UTF-8"), charge })
    }
}
impl std::ops::Deref for Text {
    type Target = str;
    fn deref(&self) -> &str { &self.value }
}
impl Drop for Text {
    fn drop(&mut self) { self.charge.owner_reference(OwnerKind::Parser, false); }
}
pub(super) struct Topics(mcap::segmented::BudgetedSegmentedVec<Text>, bool);
impl Topics {
    pub fn parse(view: budget_json::View<'_>, domain: &BudgetRef) -> Outcome<Option<Self>> {
        let Some(values) = view.array() else { return Ok(None); };
        let result = Self(mcap::segmented::BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Declaration), false);
        let mut result = result;
        for value in values {
            if let Some(text) = value.as_str() { result.0.push_fixed(Text::new(text, domain)?)?; }
        }
        result.0.sort_by(|a,b| a.value.cmp(&b.value));
        result.0.owner_reference(OwnerKind::Parser, true);
        result.1 = true;
        Ok(Some(result))
    }
    pub fn contains(&self, text: &str) -> bool {
        let (mut lo, mut hi) = (0, self.0.len());
        while lo < hi {
            let mid = lo + (hi-lo)/2;
            match self.0[mid].value.as_str().cmp(text) {
                std::cmp::Ordering::Less => lo = mid+1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }
}
impl Drop for Topics {
    fn drop(&mut self) { if self.1 { self.0.owner_reference(OwnerKind::Parser, false); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charged_filters_preserve_array_membership_and_release() {
        let domain = BudgetRef::new(mcap::storage::BudgetLimits { retained: 0, ..Default::default() }).unwrap();
        for (json, present) in [(br#"["z","a","a",null,7]"#.as_slice(),true), (b"[]",true), (b"null",false), (b"{}",false)] {
            let document=budget_json::Document::parse(json,&domain).unwrap();
            let filter=Topics::parse(document.view(),&domain).unwrap();
            assert_eq!(filter.is_some(),present);
            if let Some(filter)=&filter {
                assert_eq!(filter.contains("a"),json.len()>2);
                assert_eq!(filter.contains("z"),json.len()>2);
                assert!(!filter.contains("missing"));
            }
            drop(document); drop(filter);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],0);
        }
    }
}
