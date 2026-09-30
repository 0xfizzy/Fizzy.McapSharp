//! Stable byte storage for parsers. Shared ranges are immutable; producers only append
//! to unpublished bytes. An allocation is never compacted or reused while shared.
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub struct StorageLimit {
    pub resource: &'static str,
    pub limit: usize,
    pub requested: usize,
}
impl std::fmt::Display for StorageLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} memory limit {} exceeded by requested capacity {}",
            self.resource, self.limit, self.requested
        )
    }
}
impl std::error::Error for StorageLimit {}

#[derive(Clone, Copy, Debug)]
pub struct BudgetLimits {
    pub total: usize,
    pub block: usize,
    pub retained: usize,
}
impl Default for BudgetLimits {
    fn default() -> Self {
        Self {
            total: 256 << 20,
            block: 64 << 20,
            retained: 64 << 20,
        }
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct BudgetStatistics {
    pub current: u64,
    pub peak: u64,
    pub retained: u64,
    pub allocations: u64,
    pub copied: u64,
}
#[derive(Default)]
struct State {
    stats: BudgetStatistics,
    free: Vec<Box<[u8]>>,
}
pub struct MemoryBudget {
    limits: BudgetLimits,
    state: Mutex<State>,
    leases: std::sync::atomic::AtomicUsize,
}
impl std::fmt::Debug for MemoryBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryBudget")
            .field("limits", &self.limits)
            .finish()
    }
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self::new(BudgetLimits::default())
    }
}
impl MemoryBudget {
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(State::default()),
            leases: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    pub fn acquire_lease(&self) {
        self.leases
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub fn release_lease(&self) {
        self.leases
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub fn has_leases(&self) -> bool {
        self.leases.load(std::sync::atomic::Ordering::Acquire) != 0
    }
    pub fn statistics(&self) -> BudgetStatistics {
        self.state.lock().unwrap().stats
    }
    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }
    pub fn copied(&self, n: usize) {
        self.state.lock().unwrap().stats.copied += n as u64;
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> std::io::Result<Reservation> {
        let mut r = Reservation {
            budget: self.clone(),
            bytes: 0,
        };
        r.resize(bytes)?;
        Ok(r)
    }
    fn allocate(self: &Arc<Self>, size: usize) -> std::io::Result<Arc<Allocation>> {
        if size > self.limits.block {
            return Err(std::io::Error::other(StorageLimit {
                resource: "StorageBlock",
                limit: self.limits.block,
                requested: size,
            }));
        }
        // Bounded size classes prevent retention of arbitrarily many tiny allocations.
        let size = size
            .max(4096)
            .checked_next_power_of_two()
            .unwrap_or(self.limits.block)
            .min(self.limits.block);
        let mut s = self.state.lock().unwrap();
        if let Some(i) = s
            .free
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() >= size)
            .min_by_key(|(_, b)| b.len())
            .map(|(i, _)| i)
        {
            let bytes = s.free.swap_remove(i);
            s.stats.retained -= bytes.len() as u64;
            return Ok(Arc::new(Allocation::new(bytes, self.clone())));
        }
        while s.stats.current.saturating_add(size as u64) > self.limits.total as u64
            && !s.free.is_empty()
        {
            let b = s.free.pop().unwrap();
            s.stats.current -= b.len() as u64;
            s.stats.retained -= b.len() as u64;
        }
        if s.stats.current.saturating_add(size as u64) > self.limits.total as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "Native storage budget is occupied",
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(std::io::Error::other)?;
        bytes.resize(size, 0);
        let bytes = bytes.into_boxed_slice();
        s.stats.current += bytes.len() as u64;
        s.stats.peak = s.stats.peak.max(s.stats.current);
        s.stats.allocations += 1;
        Ok(Arc::new(Allocation::new(bytes, self.clone())))
    }
}
pub struct Reservation {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}
impl Reservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn limits(&self) -> BudgetLimits {
        self.budget.limits
    }
    pub fn domain(&self) -> Arc<MemoryBudget> {
        self.budget.clone()
    }
    pub fn resize(&mut self, bytes: usize) -> std::io::Result<()> {
        let mut s = self.budget.state.lock().unwrap();
        let requested = s.stats.current - self.bytes as u64 + bytes as u64;
        if requested > self.budget.limits.total as u64 {
            while s.stats.current - self.bytes as u64 + bytes as u64
                > self.budget.limits.total as u64
                && !s.free.is_empty()
            {
                let b = s.free.pop().unwrap();
                s.stats.current -= b.len() as u64;
                s.stats.retained -= b.len() as u64;
            }
        }
        if s.stats.current - self.bytes as u64 + bytes as u64 > self.budget.limits.total as u64 {
            return Err(std::io::Error::new(
                if bytes > self.budget.limits.total {
                    std::io::ErrorKind::Other
                } else {
                    std::io::ErrorKind::WouldBlock
                },
                StorageLimit {
                    resource: "NativeDomain",
                    limit: self.budget.limits.total,
                    requested: usize::try_from(requested).unwrap_or(usize::MAX),
                },
            ));
        }
        s.stats.current = s.stats.current - self.bytes as u64 + bytes as u64;
        s.stats.peak = s.stats.peak.max(s.stats.current);
        if bytes > self.bytes {
            s.stats.allocations += 1;
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.state.lock().unwrap().stats.current -= self.bytes as u64;
    }
}

/// Shared conservative reservation for declaration/index containers. Equality deliberately
/// ignores resource ownership so Summary retains its semantic equality contract.
#[derive(Clone, Default)]
pub(crate) struct Bookkeeping(Option<Arc<Mutex<Reservation>>>);
impl PartialEq for Bookkeeping {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Bookkeeping {}
impl Bookkeeping {
    pub fn is_charged(&self) -> bool {
        self.0.is_some()
    }
    pub fn bytes(&self) -> usize {
        self.0.as_ref().map_or(0, |r| r.lock().unwrap().bytes())
    }
    pub fn new(budget: &Arc<MemoryBudget>) -> std::io::Result<Self> {
        Ok(Self(Some(Arc::new(Mutex::new(budget.reserve(0)?)))))
    }
    pub fn grow(&self, bytes: usize) -> std::io::Result<()> {
        if let Some(r) = &self.0 {
            let mut r = r.lock().unwrap();
            let n = r
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| std::io::Error::other("Bookkeeping capacity overflow"))?;
            r.resize(n)?;
        }
        Ok(())
    }
    pub fn reserve_vec<T>(&self, v: &mut Vec<T>) -> std::io::Result<()> {
        if v.len() == v.capacity() {
            let next = v
                .capacity()
                .max(8)
                .checked_mul(2)
                .ok_or_else(|| std::io::Error::other("Index capacity overflow"))?;
            // finish() and its cached Summary may coexist; reserve their copies too.
            self.grow((next - v.capacity()) * std::mem::size_of::<T>() * 3)?;
            v.try_reserve_exact(next - v.len())
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }
}
struct Allocation {
    pointer: *mut u8,
    length: usize,
    budget: Arc<MemoryBudget>,
}
// Only WriteBuffer mutates, and only outside every published SharedBytes range.
unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}
impl Allocation {
    fn new(bytes: Box<[u8]>, budget: Arc<MemoryBudget>) -> Self {
        let length = bytes.len();
        Self {
            pointer: Box::into_raw(bytes).cast(),
            length,
            budget,
        }
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.length) }
    }
    unsafe fn tail(&self, range: Range<usize>) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.pointer.add(range.start), range.len())
    }
}
impl Drop for Allocation {
    fn drop(&mut self) {
        let bytes = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                self.pointer,
                self.length,
            ))
        };
        let mut s = self.budget.state.lock().unwrap();
        if s.stats.retained + bytes.len() as u64 <= self.budget.limits.retained as u64 {
            s.stats.retained += bytes.len() as u64;
            s.free.push(bytes);
        } else {
            s.stats.current -= bytes.len() as u64;
        }
    }
}
#[derive(Clone)]
enum Backing {
    Owned(Arc<Allocation>),
    External(Arc<dyn AsRef<[u8]> + Send + Sync>),
}
impl Backing {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(b) => b.bytes(),
            Self::External(b) => b.as_ref().as_ref(),
        }
    }
}
#[derive(Clone)]
pub struct SharedBytes {
    backing: Backing,
    range: Range<usize>,
}
impl SharedBytes {
    /// Capacity charged for pooled storage; external mappings have no native heap capacity.
    pub fn owned_capacity(&self) -> Option<usize> {
        match &self.backing {
            Backing::Owned(b) => Some(b.length),
            Backing::External(_) => None,
        }
    }
    pub fn empty() -> Self {
        Self::external(Arc::new(Vec::<u8>::new()), 0..0)
    }
    pub fn external(source: Arc<dyn AsRef<[u8]> + Send + Sync>, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= source.as_ref().as_ref().len());
        Self {
            backing: Backing::External(source),
            range,
        }
    }
    pub fn slice(&self, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= self.range.len());
        Self {
            backing: self.backing.clone(),
            range: self.range.start + range.start..self.range.start + range.end,
        }
    }
}
impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        match &self.backing {
            Backing::Owned(b) => unsafe {
                std::slice::from_raw_parts(b.pointer.add(self.range.start), self.range.len())
            },
            Backing::External(b) => &b.as_ref().as_ref()[self.range.clone()],
        }
    }
}

pub(crate) struct WriteBuffer {
    backing: Option<Backing>,
    pub budget: Arc<MemoryBudget>,
}
impl Default for WriteBuffer {
    fn default() -> Self {
        Self {
            backing: None,
            budget: Arc::new(MemoryBudget::default()),
        }
    }
}
impl WriteBuffer {
    pub fn bytes(&self) -> &[u8] {
        self.backing.as_ref().map_or(&[], |b| b.bytes())
    }
    pub fn external(&mut self, bytes: SharedBytes) {
        // Preserve a subrange without exposing bytes outside it.
        self.backing = Some(Backing::External(Arc::new(bytes)));
    }
    pub fn reserve(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        if let Some(Backing::Owned(b)) = &self.backing {
            if b.bytes().len() >= size {
                return Ok(());
            }
        }
        let allocation = self.budget.allocate(size)?;
        let old = &self.bytes()[preserve.clone()];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copied(old.len());
        self.backing = Some(Backing::Owned(allocation));
        Ok(())
    }
    pub fn relocate(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        let allocation = self.budget.allocate(size)?;
        let old = &self.bytes()[preserve];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copied(old.len());
        self.backing = Some(Backing::Owned(allocation));
        Ok(())
    }
    pub fn reset(&mut self) {
        if !matches!(&self.backing, Some(Backing::Owned(b)) if Arc::strong_count(b) == 1) {
            self.backing = None;
        }
    }
    pub fn exclusive(&self) -> bool {
        matches!(&self.backing, Some(Backing::Owned(b)) if Arc::strong_count(b) == 1)
    }
    pub unsafe fn writable(&mut self, range: Range<usize>) -> &mut [u8] {
        match self.backing.as_ref().unwrap() {
            Backing::Owned(b) => b.tail(range),
            _ => unreachable!(),
        }
    }
    pub fn share(&self, range: Range<usize>) -> SharedBytes {
        SharedBytes {
            backing: self.backing.as_ref().unwrap().clone(),
            range,
        }
    }
}
