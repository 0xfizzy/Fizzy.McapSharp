//! Owned ABI responses keep their domain charge until the caller frees them.
use super::*;
use mcap::storage::{BudgetRef, Reservation, ResourceCategory, OwnerKind};
use std::mem::{align_of, size_of, ManuallyDrop};

#[repr(C)]
struct Header { charge: ManuallyDrop<Reservation>, capacity: usize }
const _: () = assert!(align_of::<Header>() <= align_of::<usize>());
struct Buffer { words: Vec<usize>, charge: Option<Reservation>, length: usize }
impl Buffer {
    fn new(domain: &BudgetRef, length: usize) -> Outcome<Self> {
        if length == 0 { return Ok(Self {words:Vec::new(),charge:None,length:0}); }
        let bytes = size_of::<Header>().checked_add(length).ok_or("Response capacity overflow")?;
        let words = bytes.checked_add(size_of::<usize>()-1).ok_or("Response alignment overflow")? / size_of::<usize>();
        let (words, charge) = mcap::charged::vector_fixed::<usize>(domain, ResourceCategory::Scratch, words)?;
        charge.owner_reference(OwnerKind::Operation, true);
        Ok(Self {words,charge:Some(charge),length})
    }
    fn pointer(&mut self) -> *mut u8 { unsafe {self.words.as_mut_ptr().cast::<u8>().add(size_of::<Header>())} }
    fn copy(domain: &BudgetRef, data: &[u8]) -> Outcome<Self> {
        let mut buffer = Self::new(domain, data.len())?;
        if !data.is_empty() {
            unsafe {ptr::copy_nonoverlapping(data.as_ptr(), buffer.pointer(), data.len());}
            domain.copy_bytes(mcap::storage::CopyKind::Delivery, data.len());
        }
        Ok(buffer)
    }
    fn publish(self) -> (*mut u8, usize) {
        let Self {mut words, charge, length} = self;
        let Some(charge) = charge else {return (ptr::null_mut(),0)};
        let header = Header {charge:ManuallyDrop::new(charge),capacity:words.capacity()};
        let pointer = words.as_mut_ptr().cast::<u8>();
        unsafe {pointer.cast::<Header>().write(header);}
        std::mem::forget(words);
        (unsafe {pointer.add(size_of::<Header>())},length)
    }
}
// Field order frees words before the reservation on unpublished failure paths.
pub(super) unsafe fn free(pointer: *mut u8) {
    if pointer.is_null() {return;}
    let allocation = pointer.sub(size_of::<Header>());
    let header = allocation.cast::<Header>().read();
    let charge = ManuallyDrop::into_inner(header.charge);
    let release = charge.domain().begin_capacity_release();
    drop(Vec::<usize>::from_raw_parts(allocation.cast(),0,header.capacity));
    drop(charge);
    drop(release);
}
#[derive(Default)]
struct Counter { length: usize, overflow: bool }
impl Write for Counter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Some(length) = self.length.checked_add(data.len()) {self.length=length;} else {self.overflow=true;}
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {Ok(())}
}
struct Fill { pointer: *mut u8, remaining: usize, overflow: bool }
impl Write for Fill {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.is_empty() {return Ok(0);}
        if data.len() > self.remaining {self.overflow=true;} else {
            unsafe {ptr::copy_nonoverlapping(data.as_ptr(),self.pointer,data.len());self.pointer=self.pointer.add(data.len());}
            self.remaining-=data.len();
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {Ok(())}
}
pub(super) fn write(out: &mut Response, domain: &BudgetRef,
    serialize: impl Fn(&mut dyn Write) -> Outcome<()>, data: &[u8], value: u64) -> Outcome<()> {
    memory::check(domain,"StorageBlock",Some(domain.limits().block as u64),data.len())?;
    let mut count=Counter::default();serialize(&mut count)?;
    if count.overflow {return Err("Response capacity overflow".into());}
    let mut json=Buffer::new(domain,count.length)?;
    let mut fill=Fill {pointer:if count.length==0 {ptr::null_mut()} else {json.pointer()},remaining:count.length,overflow:false};
    serialize(&mut fill)?;
    if fill.overflow || fill.remaining!=0 {return Err("Response serialization length changed".into());}
    let data=Buffer::copy(domain,data)?;
    (out.json,out.json_len)=json.publish();
    (out.data,out.data_len)=data.publish();
    out.value=value;
    Ok(())
}

fn schema_json(writer: &mut dyn Write, schema: &mcap::shared_declarations::SharedSchema) -> Outcome<()> {
    writer.write_all(b"{\"id\":")?;serde_json::to_writer(&mut *writer,&schema.id)?;
    writer.write_all(b",\"name\":")?;serde_json::to_writer(&mut *writer,&schema.name)?;
    writer.write_all(b",\"encoding\":")?;serde_json::to_writer(&mut *writer,&schema.encoding)?;
    writer.write_all(b"}")?;Ok(())
}
pub(super) fn schema(out: &mut Response, domain: &BudgetRef, schema: &mcap::shared_declarations::SharedSchema) -> Outcome<()> {
    write(out,domain,|writer| schema_json(writer,schema),&schema.data,0)
}
pub(super) fn channel(out: &mut Response, domain: &BudgetRef, channel: &mcap::shared_declarations::SharedChannel, nested: bool) -> Outcome<()> {
    write(out,domain,|writer| {
        writer.write_all(b"{\"id\":")?;serde_json::to_writer(&mut *writer,&channel.id)?;
        writer.write_all(b",\"topic\":")?;serde_json::to_writer(&mut *writer,&channel.topic)?;
        writer.write_all(b",\"messageEncoding\":")?;serde_json::to_writer(&mut *writer,&channel.message_encoding)?;
        writer.write_all(b",\"metadata\":{")?;
        for (i,(key,value)) in channel.metadata.iter().enumerate() {
            if i!=0 {writer.write_all(b",")?;}
            serde_json::to_writer(&mut *writer,key)?;writer.write_all(b":")?;serde_json::to_writer(&mut *writer,value)?;
        }
        writer.write_all(b"},")?;
        if nested {
            writer.write_all(b"\"schema\":")?;
            if let Some(schema)=&channel.schema {schema_json(writer,schema)?;} else {writer.write_all(b"null")?;}
        } else {
            writer.write_all(b"\"schemaId\":")?;serde_json::to_writer(&mut *writer,&channel.schema_id)?;
        }
        writer.write_all(b"}")?;Ok(())
    }, if nested {channel.schema.as_ref().map(|s|s.data.as_ref()).unwrap_or_default()} else {&[]},0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_is_atomic_when_serializer_changes_or_fails() {
        let domain=BudgetRef::new(Default::default()).unwrap();
        for grows in [false,true] {
            let calls=std::cell::Cell::new(0);
            let mut output=Response::default();
            let result=write(&mut output,&domain,|writer| {
                let call=calls.get();calls.set(call+1);
                if call==0 {writer.write_all(b"a")?;} else if grows {writer.write_all(b"too long")?;} else {return Err("serialization failed".into());}
                Ok(())
            },&[1,2,3],0);
            assert!(result.is_err());
            assert!(output.json.is_null() && output.data.is_null());
            assert_eq!(domain.workload_statistics().current,0);
        }
        let mut output=Response::default();
        write(&mut output,&domain,|writer| {writer.write_all(&[])?;Ok(())},&[],0).unwrap();
        assert!(output.json.is_null() && output.data.is_null());
        assert_eq!(domain.workload_statistics().current,0);
    }
}
