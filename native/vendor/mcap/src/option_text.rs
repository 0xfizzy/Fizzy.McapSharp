//! Retained option text with explicit allocation and shared clone ownership.
use crate::{charged::{ChargedTree,TreeOwnership},storage::{BudgetRef,Reservation,ResourceCategory,OwnerKind,CopyKind,StorageFailure}};
struct Storage { value:String, charge:Reservation }
impl TreeOwnership for Storage {
    fn reference_children(&self,owner:OwnerKind,acquire:bool) {self.charge.owner_reference(owner,acquire);}
    fn mutation_owner(&mut self,_:Option<OwnerKind>) {}
}
#[derive(Clone)]
pub(crate) struct SharedText(ChargedTree<Storage>);
#[derive(Clone)]
pub(crate) enum Text { Borrowed(&'static str), Owned(String), Charged(SharedText) }
impl Text {
    pub fn new(value:&str,domain:&BudgetRef)->Result<Self,StorageFailure> {
        if value.is_empty() {return Ok(Self::Borrowed(""));}
        let (mut bytes,charge)=crate::charged::vector_fixed(domain,ResourceCategory::Declaration,value.len())?;
        bytes.extend_from_slice(value.as_bytes());domain.copy_bytes(CopyKind::Other,value.len());
        let storage=Storage {value:String::from_utf8(bytes).expect("valid UTF8"),charge};
        Ok(Self::Charged(SharedText(ChargedTree::new_owned_fixed(storage,domain,ResourceCategory::Declaration,OwnerKind::Operation)?)))
    }
    pub fn belongs_to(&self,domain:&BudgetRef)->bool {
        match self {Self::Charged(value)=>BudgetRef::ptr_eq(&value.0.charge.domain(),domain),_=>true}
    }
}
impl std::ops::Deref for Text {
    type Target=str;
    fn deref(&self)->&str {match self {Self::Borrowed(value)=>value,Self::Owned(value)=>value,Self::Charged(value)=>&value.0.value}}
}
impl std::fmt::Debug for Text {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {std::fmt::Debug::fmt(&**self,f)}}
