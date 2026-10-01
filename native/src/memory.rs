use super::*;
use std::ops::Deref;

#[derive(Clone)]
pub struct Options {
    pub domain: mcap::storage::BudgetRef,
    pub owned: Option<u64>,
    pub pending: Option<u64>,
    pub sort: Option<u64>,
    pub retained: u64,
    pub random: u64,
    pub scratch: Option<u64>,
}
impl Default for Options {
    fn default() -> Self {
        Self::with_domain(Default::default())
    }
}
impl Options {
    pub fn try_default() -> Outcome<Self> { Ok(Self::with_domain(budget::resolve(0)?)) }
    pub fn with_domain(domain: mcap::storage::BudgetRef) -> Self {
        Self {
            domain,
            random: 0,
            scratch: None,
            owned: None,
            pending: None,
            sort: None,
            retained: 8 * 1024 * 1024,
        }
    }
}
impl Options {
    pub fn parse_config(input:&[u8]) -> Outcome<Self> {
        let (document,domain)=budget_json::Document::configured(input,&["Budget","id"])?;
        Self::parse_view(document.view(),domain)
    }
    pub fn parse_view(v:budget_json::View<'_>,domain:mcap::storage::BudgetRef)->Outcome<Self> {
        fn field(v:budget_json::View<'_>,key:&str)->Outcome<Option<u64>> {
            let value=v.get(key);
            if value.is_null() {Ok(None)} else {Ok(Some(value.as_u64().ok_or("Invalid memory limit")?))}
        }
        Ok(Self {
            domain,
            random:field(v,"MaxRandomAccessCacheBytes")?.unwrap_or(0),
            scratch:field(v,"MaxScratchBufferBytes")?,
            owned:field(v,"MaxOwnedInputBytes")?,
            pending:field(v,"MaxPendingBufferBytes")?,
            sort:field(v,"MaxBufferedSortBytes")?,
            retained:field(v,"MaxRetainedBufferBytes")?.unwrap_or(8*1024*1024),
        })
    }
    pub fn parse(v: &Value) -> Outcome<Self> {
        fn field(v: &Value, key: &str) -> Outcome<Option<u64>> {
            if v[key].is_null() {
                Ok(None)
            } else {
                Ok(Some(v[key].as_u64().ok_or("Invalid memory limit")?))
            }
        }
        Ok(Self {
            domain: budget::parse(&v["Budget"] )?,
            random: field(v, "MaxRandomAccessCacheBytes")?.unwrap_or(0),
            scratch: field(v, "MaxScratchBufferBytes")?,
            owned: field(v, "MaxOwnedInputBytes")?,
            pending: field(v, "MaxPendingBufferBytes")?,
            sort: field(v, "MaxBufferedSortBytes")?,
            retained: field(v, "MaxRetainedBufferBytes")?.unwrap_or(8 * 1024 * 1024),
        })
    }
}
#[derive(Debug)]
pub struct Limit {
    pub resource: &'static str,
    pub limit: u64,
    pub domain_limit: u64,
    pub current: u64,
    pub requested: u64,
}
impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} memory limit {} exceeded by requested capacity {}",
            self.resource, self.limit, self.requested
        )
    }
}
impl std::error::Error for Limit {}
pub fn check(domain: &mcap::storage::BudgetRef, resource: &'static str, limit: Option<u64>, requested: usize) -> Outcome<()> {
    if let Some(limit) = limit {
        if requested as u64 > limit {
            return Err(Limit {
                resource,
                limit,
                domain_limit: domain.limits().total as u64,
                current: domain.statistics().current,
                requested: requested as u64,
            }.into());
        }
    }
    if requested > isize::MAX as usize {
        return Err(mcap::storage::StorageFailure {
            details: mcap::storage::StorageLimit {
                resource, limit: isize::MAX as usize, domain_limit: domain.limits().total,
                requested, current: domain.statistics().current as usize, phase: "resource-check",
            },
            kind: mcap::storage::StorageFailureKind::Overflow, terminal: false,
        }.into());
    }
    Ok(())
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Statistics {
    pub current: u64,
    pub peak: u64,
    pub allocations: u64,
    pub copied: u64,
    pub mapped: u64,
}
impl Statistics {
    pub fn capacity(&mut self, old: usize, new: usize) {
        self.current = self.current - old as u64 + new as u64;
        self.peak = self.peak.max(self.current);
        if new > old {
            self.allocations += 1;
        }
    }
    pub fn with_input(mut self, input: &Backing) -> Self {
        let n = input.capacity() as u64;
        self.current += n;
        self.peak += n;
        self.allocations += u64::from(n != 0);
        self.copied += if n != 0 { input.len() as u64 } else { 0 };
        self.mapped = input.mapped();
        self
    }
}
pub enum Backing {
    Empty,
    Owned { data: Vec<u8>, _charge: mcap::storage::Reservation },
    Mapped { mapping: mcap::charged::ChargedShared<io::MappedInput> },
}
impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Owned {data,..} => data,
            Self::Mapped { mapping, .. } => mapping,
        }
    }
}
impl mcap::storage::SharedSource for Backing {
    fn owner_reference(&self, kind: mcap::storage::OwnerKind, acquire: bool) {
        match self {
            Self::Owned { _charge,.. } => _charge.owner_reference(kind,acquire),
            Self::Mapped { mapping } => mapping.charge_owner(kind,acquire),
            Self::Empty => {},
        }
    }
    fn reference(&self,cache:bool,acquire:bool){if let Self::Owned{_charge,..}=self{_charge.reference(cache,acquire);}}
}
impl AsRef<[u8]> for Backing { fn as_ref(&self) -> &[u8] { self } }
impl Backing {
    pub fn owned(data: Vec<u8>, charge: mcap::storage::Reservation) -> Self {
        Self::Owned { data, _charge: charge }
    }
    pub fn capacity(&self) -> usize {
        match self {
            Self::Owned {data,..} => data.capacity(),
            _ => 0,
        }
    }
    pub fn mapped(&self) -> u64 {
        match self {
            Self::Mapped { mapping, .. } => mapping.len() as u64,
            _ => 0,
        }
    }
    pub fn copy(data: &[u8], options: Options) -> Outcome<Self> {
        check(&options.domain, "OwnedInput", options.owned, data.len())?;
        check(&options.domain, "StorageBlock", Some(options.domain.limits().block as u64), data.len())?;
        let (mut v, charge)=mcap::charged::vector_fixed(&options.domain,mcap::storage::ResourceCategory::Input,data.len())?;
        v.extend_from_slice(data);
        options.domain.copy_bytes(mcap::storage::CopyKind::Input,data.len());
        Ok(Self::owned(v, charge))
    }
    pub fn open(path: &str, domain: &mcap::storage::BudgetRef) -> Outcome<Self> {
        match open_input(path, domain)? {
            Input::Map { mapping, .. } => Ok(Self::Mapped { mapping: mapping.into_shared() }),
            _ => unreachable!(),
        }
    }
}
/// One reader/snapshot input reference. Payload slices retain storage separately,
/// so disposing the input owner releases its parser pin even when a lease survives.
pub struct Source(Option<mcap::charged::ChargedShared<Backing>>);
static EMPTY_BACKING: Backing = Backing::Empty;
impl Source {
    pub fn empty() -> Self { Self(None) }
    pub fn new(backing: Backing, domain: &mcap::storage::BudgetRef) -> Outcome<Self> {
        let same_domain=match &backing {
            Backing::Empty => return Ok(Self::empty()),
            Backing::Owned { _charge,.. } => mcap::storage::BudgetRef::ptr_eq(&_charge.domain(),domain),
            Backing::Mapped { mapping } => mapping._registration.belongs_to(domain),
        };
        if !same_domain { return Err("Input storage cannot change memory budget domains".into()); }
        let root=mcap::charged::ChargedShared::new_fixed(backing,domain,mcap::storage::ResourceCategory::Scratch)?;
        let source=Self(Some(root));
        source.reference(true);
        Ok(source)
    }
    fn reference(&self, acquire: bool) {
        use mcap::storage::{SharedSource, OwnerKind};
        if let Some(root)=&self.0 {
            root.charge_owner(OwnerKind::Parser,acquire);
            root.owner_reference(OwnerKind::Parser,acquire);
        }
    }
    pub fn shared(&self, range: std::ops::Range<usize>) -> mcap::storage::SharedBytes {
        if let Some(root)=&self.0 {
            mcap::storage::SharedBytes::charged_external(root.clone().into_source(),range)
        } else {
            assert_eq!(range,0..0);
            mcap::storage::SharedBytes::empty()
        }
    }
}
impl Clone for Source {
    fn clone(&self) -> Self {
        let source=Self(self.0.clone());
        source.reference(true);
        source
    }
}
impl Drop for Source {
    fn drop(&mut self) { self.reference(false); }
}
impl Deref for Source {
    type Target = Backing;
    fn deref(&self) -> &Backing { self.0.as_deref().unwrap_or(&EMPTY_BACKING) }
}
// A synchronous, private delivery callback. Never retained after an export returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sink {
    pub context: *mut std::ffi::c_void,
    pub accept: unsafe extern "C" fn(*mut std::ffi::c_void, u8, *const MessageHeader, *const u8, usize, *mut usize) -> i32,
}
const _: () = assert!(std::mem::size_of::<Sink>() == 16);
impl Sink {
    pub unsafe fn send(self, opcode: u8, header: &MessageHeader, data: &[u8]) -> Outcome<u64> {
        let mut copied = 0;
        if (self.accept)(self.context, opcode, header, data.as_ptr(), data.len(), &mut copied) != 0 {
            return Err("Managed delivery callback failed".into());
        }
        if copied > data.len() { return Err("Invalid delivery copy count".into()); }
        Ok(copied as u64)
    }
}
pub struct Delivery {
    pub retry_capacity: usize,
    pub charge: Option<mcap::storage::Reservation>,
    pub shared: Option<mcap::storage::SharedBytes>,
    pub capture: bool,
    pub sink: Option<Sink>,
    pub wanted: u8,
    pub data: Vec<u8>,
    pub active: bool,
    pub opcode: u8,
    pub header: MessageHeader,
    pub options: Options,
    pub stats: Statistics,
}
impl Default for Delivery {
    fn default() -> Self {
        Self::new(Options::default())
    }
}
impl Delivery {
    pub fn new(options: Options) -> Self {
        Self {
            retry_capacity: 0, charge: None, shared: None, capture: false,
            sink: None,
            wanted: 0,
            data: Vec::new(),
            active: false,
            opcode: 0,
            header: MessageHeader::default(),
            options,
            stats: Statistics::default(),
        }
    }
}
impl Delivery {
    pub fn take_shared(&mut self, start:usize) -> Outcome<mcap::storage::SharedBytes> {
        if let Some(shared) = self.shared.as_ref() {
            if start > shared.as_ref().len() { return Err("Payload range exceeds pending data".into()); }
            let mut result = shared.slice(start..shared.as_ref().len());
            result.set_owner(mcap::storage::OwnerKind::Operation);
            self.shared = None;
            self.active = false;
            return Ok(result);
        }
        let n=self.data.len();
        if start>n { return Err("Payload range exceeds pending data".into()); }
        // Allocate the exact control before transferring any pending data or charge.
        // A refusal leaves the original delivery available for a safe retry.
        let mut data=mcap::charged::ChargedShared::new_fixed(Backing::Empty,&self.options.domain,mcap::storage::ResourceCategory::Scratch)?;
        let charge=match self.charge.take() { Some(c)=>c,None=>self.options.domain.try_reserve(self.data.capacity(),mcap::storage::ResourceCategory::Scratch)? };
        self.stats.capacity(self.data.capacity(),0);
        *data.get_mut().unwrap()=Backing::owned(std::mem::take(&mut self.data),charge);
        self.active=false;
        Ok(mcap::storage::SharedBytes::charged_external(data.into_source(),start..n))
    }
    pub fn pending_len(&self) -> usize {
        self.shared.as_ref().map_or(self.data.len(), |data| data.as_ref().len())
    }
    pub fn retain_pending(&mut self, logical_length: usize) -> Outcome<()> {
        check(&self.options.domain, "PendingBuffer", self.options.pending, logical_length)?;
        if let Some(shared) = self.shared.as_mut() {
            shared.set_owner(mcap::storage::OwnerKind::Pending);
        }
        self.active = true;
        Ok(())
    }
    pub fn record_delivery(&mut self, copied: usize) {
        self.stats.copied += copied as u64;
        self.options.domain.copy_bytes(mcap::storage::CopyKind::Delivery, copied);
    }
    pub fn reserve(&mut self, n: usize) -> Outcome<()> {
        self.reserve_resource(n, "PendingBuffer", self.options.pending, mcap::storage::ResourceCategory::Scratch)
    }
    pub fn reserve_scratch(&mut self, n: usize) -> Outcome<()> {
        self.reserve_resource(n, "ScratchBuffer", self.options.scratch, mcap::storage::ResourceCategory::Scratch)
    }
    pub fn reserve_input(&mut self, n: usize) -> Outcome<()> {
        // Stable indexed input is transferred after each read, never reused in place.
        self.discard();
        self.reserve_resource(n, "ScratchBuffer", self.options.scratch, mcap::storage::ResourceCategory::Input)
    }
    fn reserve_resource(&mut self, n: usize, resource: &'static str, limit: Option<u64>, category: mcap::storage::ResourceCategory) -> Outcome<()> {
        if n > self.data.capacity() {
            check(&self.options.domain, resource, limit, n)?;
            check(&self.options.domain, "StorageBlock",Some(self.options.domain.limits().block as u64),n)?;
            let old=self.data.capacity();
            let (mut replacement, charge)=mcap::charged::vector_fixed(&self.options.domain,category,n)?;
            replacement.extend_from_slice(&self.data);
            self.options.domain.copy_bytes(mcap::storage::CopyKind::Compaction,self.data.len());
            self.data=replacement;
            self.charge=Some(charge);
            self.stats.capacity(old,n);
            if old != 0 { self.options.domain.reallocated(); }
        }
        Ok(())
    }
    pub fn release(&mut self) {
        self.shared = None;
        self.active = false;
        self.data.clear();
        if self.data.capacity() as u64 > self.options.retained {
            self.discard();
        }
    }
    pub fn discard(&mut self) {
        self.shared = None;
        self.stats.capacity(self.data.capacity(), 0);
        self.data = Vec::new();
        self.charge=None;
        self.active = false;
    }
    pub unsafe fn send(&mut self, data: &[u8], dest: *mut u8) -> Outcome<()> {
        if let Some(sink) = self.sink {
            let copied=sink.send(self.opcode, &self.header, data)?;
            self.record_delivery(copied as usize);
        } else {
            copy(data, dest)?;
            self.record_delivery(data.len());
        }
        Ok(())
    }
    pub unsafe fn deliver(&mut self, data: &[u8], dest: *mut u8, capacity: usize) -> Outcome<i32> {
        if self.capture { return Ok(0); }
        if self.sink.is_some() {
            self.send(data, dest)?;
            self.release();
            return Ok(0);
        }
        if capacity < data.len() {
            self.retain_pending(data.len())?;
            if self.shared.is_some() { return Ok(2); }
            self.data.clear();
            self.reserve(data.len())?;
            self.data.extend_from_slice(data);
            self.stats.copied += data.len() as u64;self.options.domain.copy_bytes(mcap::storage::CopyKind::Other,data.len());
            self.active = true;
            return Ok(2);
        }
        copy(data, dest)?;
        self.record_delivery(data.len());
        self.release();
        Ok(0)
    }
    pub unsafe fn retry(&mut self, dest: *mut u8, capacity: usize) -> Outcome<i32> {
        self.retry_range(0, dest, capacity)
    }
    pub unsafe fn retry_range(&mut self, start: usize, dest: *mut u8, capacity: usize) -> Outcome<i32> {
        let length = self.pending_len();
        if start > length { return Err("Payload range exceeds pending data".into()); }
        if self.capture {
            self.shared=Some(self.take_shared(start)?); return Ok(0);
        }
        let data = self.shared.as_ref().map_or(self.data.as_slice(), |data| data.as_ref());
        let data = &data[start..];
        let copied = if let Some(sink) = self.sink {
            sink.send(self.opcode, &self.header, data)? as usize
        } else {
            if capacity < data.len() { return Ok(2); }
            copy(data, dest)?;
            data.len()
        };
        self.record_delivery(copied);
        self.release();
        Ok(0)
    }

}
pub unsafe fn copy(data: &[u8], dest: *mut u8) -> Outcome<()> {
    if !data.is_empty() {
        if dest.is_null() {
            return Err("Null destination".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
    }
    Ok(())
}

#[derive(Default)]
pub struct Measure {
    position: u64,
    pub length: u64,
}
impl std::io::Write for Measure {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.position = self
            .position
            .checked_add(data.len() as u64)
            .ok_or_else(|| std::io::Error::other("Encoding size overflow"))?;
        self.length = self.length.max(self.position);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Seek for Measure {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let n = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => self.length as i128 + n as i128,
        };
        self.position =
            u64::try_from(n).map_err(|_| std::io::Error::other("Encoding offset overflow"))?;
        Ok(self.position)
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_memory_statistics(
    kind: u32,
    p: *const std::ffi::c_void,
    stats: *mut Statistics,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if p.is_null() || stats.is_null() {
            return Err("Null memory statistics argument".into());
        }
        *stats = match kind {
            0 => {
                let r = &*(p as *const Reader);
                let mut s = r.delivery.stats;
                s.current += r.scratch.stats.current;
                s.allocations += r.scratch.stats.allocations;
                s.copied += r.scratch.stats.copied;
                s.current += r.arena.stats.current;
                s.peak = r.memory_peak.max(s.current);
                s.allocations += r.arena.stats.allocations;
                s.copied += r.arena.stats.copied;
                if let Input::Map { mapping, .. } = &r.input {
                    s.mapped = mapping.len() as u64;
                }
                s
            }
            1 => {
                let r = &*(p as *const buffer_reader::BufferReader);
                r.delivery.stats.with_input(&r.input)
            }
            2 => {
                let r = &*(p as *const extended::Snapshot);
                {
                    let mut s = Statistics::default();
                    s.current += r.stats.current + r.cache.resident_bytes();
                    s.allocations += r.cache.stats.allocations;
                    s.copied += r.cache.stats.copied;
                    s.peak = r.memory_peak.max(s.current);
                    s.allocations += r.stats.allocations;
                    s.copied += r.stats.copied;
                    s.with_input(&r.data)
                }
            }
            3 => (&*(p as *const extended::EngineHandle)).delivery.stats,
            _ => return Err("Unknown memory statistics source".into()),
        };
        Ok(0)
    })
}

const _: () = assert!(std::mem::size_of::<Statistics>() == 40);
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_capacity_and_reusable_delivery() {
        assert!(check(&Default::default(), "PendingBuffer", None, usize::MAX).is_err());
        let mut d = Delivery::default();
        d.options.pending = Some(16);
        unsafe {
            assert_eq!(d.deliver(&[1; 16], ptr::null_mut(), 0).unwrap(), 2);
            let counters = d.stats;
            assert_eq!(d.retry(ptr::null_mut(), 0).unwrap(), 2);
            assert_eq!(d.stats.allocations, counters.allocations);
            let mut output = [0; 16];
            assert_eq!(d.retry(output.as_mut_ptr(), 16).unwrap(), 0);
            assert_eq!(output, [1; 16]);
            assert_eq!(d.deliver(&[2; 16], ptr::null_mut(), 0).unwrap(), 2);
            assert_eq!(d.stats.allocations, counters.allocations);
            d.release();
            assert!(d.deliver(&[3; 17], ptr::null_mut(), 0).is_err());
            d.discard();
            assert_eq!(d.stats.current, 0);
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use mcap::storage::OwnerKind;
    #[test]
    fn shared_pending_has_an_explicit_pin_and_transfers_without_copy() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let options = Options { domain: domain.clone(), pending: Some(32), ..Default::default() };
        let source = Source::new(Backing::copy(&[7; 32], options.clone()).unwrap(), &domain).unwrap();
        let mut delivery = Delivery { options, shared: Some(source.shared(0..32)), ..Default::default() };
        let initial_copy = domain.workload_detailed_statistics().flow;
        unsafe { assert_eq!(delivery.deliver(&[7;32], ptr::null_mut(), 0).unwrap(),2); }
        assert!(domain.ownership_statistics().bytes[OwnerKind::Pending as usize] >= 32);
        drop(source);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],0);
        assert!(delivery.data.is_empty());
        let mut shared = delivery.take_shared(3).unwrap();
        assert_eq!(shared.as_ref(), &[7;29]);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Pending as usize],0);
        shared.set_owner(OwnerKind::Lease);
        assert_eq!(domain.workload_detailed_statistics().flow.delivery_copy, initial_copy.delivery_copy);
        assert_eq!(domain.workload_detailed_statistics().flow.other_copy, initial_copy.other_copy);
        drop(shared);
        assert_eq!(domain.workload_statistics().current,0);
    }
    #[test]
    fn shared_pending_limit_refusal_and_discard_release_the_owner() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let options = Options { domain: domain.clone(), pending: Some(31), ..Default::default() };
        let source = Source::new(Backing::copy(&[7; 32], options.clone()).unwrap(), &domain).unwrap();
        let mut delivery = Delivery { options, shared: Some(source.shared(0..32)), ..Default::default() };
        drop(source);
        unsafe { assert!(delivery.deliver(&[7;32], ptr::null_mut(), 0).is_err()); }
        assert!(!delivery.active);
        delivery.discard();
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
    #[test]
    fn mapping_registration_survives_reader_and_is_unique_across_slices() {
        let path = std::env::temp_dir().join(format!("fizzy-mapping-ledger-{}.bin", std::process::id()));
        std::fs::write(&path, [42; 128]).unwrap();
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let source = Source::new(Backing::open(path.to_str().unwrap(), &domain).unwrap(), &domain).unwrap();
        let other = source.clone();
        let lease = source.shared(4..8).clone_for(OwnerKind::Lease);
        assert_eq!(domain.mapped_logical_bytes(), 128);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Lease as usize], domain.workload_statistics().current);
        assert_eq!(domain.workload_detailed_statistics().lease_payload_bytes, 0);
        drop(source);
        drop(other);
        assert_eq!(domain.mapped_logical_bytes(), 128);
        assert_eq!(lease.as_ref(), &[42; 4]);
        drop(lease);
        assert_eq!(domain.mapped_logical_bytes(), 0);
        assert_eq!(domain.workload_statistics().current, 0);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn refused_input_control_and_domain_change_release_transferred_input() {
        let domain=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits { total:(128) + mcap::storage::BudgetRef::allocation_size(), block:128, retained:0 }).unwrap();
        let options=Options { domain:domain.clone(), ..Default::default() };
        let backing=Backing::copy(&[7;128],options.clone()).unwrap();
        assert!(Source::new(backing,&domain).is_err());
        assert_eq!(domain.workload_statistics().current,0);
        let backing=Backing::copy(&[7;128],options).unwrap();
        let other=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        assert!(Source::new(backing,&other).is_err());
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(other.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
    #[test]
    fn input_owner_pin_ends_before_last_payload_lease() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let options = Options { domain: domain.clone(), ..Default::default() };
        let source = Source::new(Backing::copy(&[7; 128], options).unwrap(), &domain).unwrap();
        let capacity=domain.workload_statistics().current;
        let second = source.clone();
        let lease = source.shared(32..64).clone_for(OwnerKind::Lease);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize], capacity);
        assert_eq!(domain.ownership_statistics().externally_releasable, 0);
        drop(source);
        assert_eq!(domain.ownership_statistics().externally_releasable, 0);
        drop(second);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize], 0);
        assert_eq!(domain.ownership_statistics().externally_releasable, capacity);
        assert_eq!(lease.as_ref(), &[7; 32]);
        drop(lease);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn refused_shared_control_preserves_pending_payload_for_retry() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut delivery = Delivery::default();
        delivery.options.domain = domain.clone();
        delivery.reserve(128).unwrap();
        delivery.data.resize(128, 42);
        delivery.active = true;
        let current = domain.workload_statistics().current;
        let blocker = domain.reserve(domain.limits().total - domain.statistics().current as usize).unwrap();
        assert!(delivery.take_shared(3).is_err());
        assert_eq!(delivery.data, [42;128]);
        assert!(delivery.active);
        assert!(delivery.charge.is_some());
        drop(blocker);
        assert_eq!(domain.workload_statistics().current, current);
        let shared = delivery.take_shared(3).unwrap();
        assert_eq!(shared.as_ref(), &[42;125]);
        assert!(delivery.data.is_empty());
        assert!(!delivery.active);
        drop(shared);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn scratch_transfer_does_not_leave_a_permanent_parser_pin() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut delivery = Delivery::default();
        delivery.options.domain = domain.clone();
        delivery.reserve(128).unwrap();
        delivery.data.resize(128, 42);
        let mut shared = delivery.take_shared(0).unwrap();
        shared.set_owner(OwnerKind::Lease);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize], 0);
        assert!(domain.ownership_statistics().externally_releasable >= 128);
        drop(shared);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
