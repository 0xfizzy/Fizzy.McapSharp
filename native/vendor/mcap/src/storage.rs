//! Stable byte storage for parsers. Shared ranges are immutable; producers only append
//! to unpublished bytes. An allocation is never compacted or reused while shared.
use std::{
    ops::Range,
    sync::{Arc, Mutex, Weak},
};

#[derive(Debug)]
pub struct StorageLimit {
    pub resource: &'static str,
    pub limit: usize,
    pub requested: usize,
    pub current: usize,
    pub phase: &'static str,
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
#[repr(usize)]
#[derive(Clone, Copy, Debug)]
pub enum ResourceCategory {
    Input,
    Decompressed,
    Writer,
    CodecEncoder,
    CodecDecoder,
    Index,
    Descriptor,
    Declaration,
    Scratch,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct ResourceStatistics {
    pub current: u64,
    pub peak: u64,
    pub live: u64,
    pub reserved: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct DetailedStatistics {
    pub resources: [ResourceStatistics; 9],
    pub allocation_count: u64,
    pub allocated_bytes: u64,
    pub rejected: u64,
    pub flow: FlowStatistics,
    pub lease_payload_bytes: u64,
    pub cache_payload_bytes: u64,
    pub current_bytes: u64,
    pub peak_bytes: u64,
    pub idle_bytes: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct FlowStatistics {
    pub input_copy: u64,
    pub compaction_copy: u64,
    pub delivery_copy: u64,
    pub other_copy: u64,
    pub encoded_input: u64,
    pub encoded_output: u64,
    pub decoded_input: u64,
    pub decoded_output: u64,
    pub decode_started: u64,
    pub decode_completed: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_evictions: u64,
}
#[derive(Clone, Copy)]
pub enum CopyKind {
    Input,
    Compaction,
    Delivery,
    Other,
}
#[derive(Default)]
struct State {
    stats: BudgetStatistics,
    detailed: DetailedStatistics,
    free: Vec<(Box<[u8]>, ResourceCategory)>,
}
impl State {
    fn add_live(&mut self, category: ResourceCategory, bytes: usize, new: bool) {
        let c = &mut self.detailed.resources[category as usize];
        c.current += bytes as u64;
        c.live += bytes as u64;
        c.peak = c.peak.max(c.current);
        if new {
            self.detailed.allocation_count += 1;
            self.detailed.allocated_bytes += bytes as u64;
        }
    }
    fn remove_live(&mut self, category: ResourceCategory, bytes: usize) {
        let c = &mut self.detailed.resources[category as usize];
        c.current -= bytes as u64;
        c.live -= bytes as u64;
    }
}
pub trait Reclaimable: Send + Sync {
    fn last_access(&self) -> u64;
    fn reclaim(&self) -> bool;
}
#[derive(Default)]
struct ReclaimerRegistry {
    entries: Vec<Weak<dyn Reclaimable>>,
    _charge: Option<DetachedReservation>,
}
struct DetachedReservation {
    budget: Weak<MemoryBudget>,
    bytes: usize,
    live: usize,
    category: ResourceCategory,
}
impl Drop for DetachedReservation {
    fn drop(&mut self) {
        if let Some(budget) = self.budget.upgrade() {
            let mut s = budget.state.lock().unwrap();
            s.stats.current -= self.bytes as u64;
            let c = &mut s.detailed.resources[self.category as usize];
            c.current -= self.bytes as u64;
            c.live -= self.live as u64;
            c.reserved = c.current - c.live;
            if self.bytes != 0 {
                budget.capacity_changed();
            }
        }
    }
}
#[derive(Default)]
struct ReferenceCounts {
    leases: std::sync::atomic::AtomicUsize,
    caches: std::sync::atomic::AtomicUsize,
}
impl ReferenceCounts {
    fn update(&self, budget: &MemoryBudget, bytes: usize, cache: bool, acquire: bool) {
        let mut state = budget.state.lock().unwrap();
        let counter = if cache { &self.caches } else { &self.leases };
        let value = if acquire {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
        };
        let total = if cache {
            &mut state.detailed.cache_payload_bytes
        } else {
            &mut state.detailed.lease_payload_bytes
        };
        if acquire && value == 0 {
            *total += bytes as u64;
        } else if !acquire && value == 1 {
            *total -= bytes as u64;
        }
    }
}
pub trait SharedSource: AsRef<[u8]> + Send + Sync {
    fn reference(&self, _cache: bool, _acquire: bool) {}
}
impl SharedSource for Vec<u8> {}
pub struct MemoryBudget {
    limits: BudgetLimits,
    state: Mutex<State>,
    leases: std::sync::atomic::AtomicUsize,
    reclaimers: Mutex<ReclaimerRegistry>,
    clock: std::sync::atomic::AtomicU64,
    capacity_version: std::sync::atomic::AtomicU64,
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
            reclaimers: Mutex::new(ReclaimerRegistry::default()),
            clock: std::sync::atomic::AtomicU64::new(1),
            capacity_version: std::sync::atomic::AtomicU64::new(0),
        }
    }
    pub fn capacity_version(&self) -> u64 {
        self.capacity_version
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn capacity_changed(&self) {
        self.capacity_version
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }
    pub fn touch(&self) -> u64 {
        self.clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    pub fn register_reclaimer(
        self: &Arc<Self>,
        item: Weak<dyn Reclaimable>,
    ) -> std::io::Result<()> {
        loop {
            let capacity = {
                let mut registry = self.reclaimers.lock().unwrap();
                registry.entries.retain(|e| e.strong_count() != 0);
                if registry.entries.len() < registry.entries.capacity() {
                    registry.entries.push(item);
                    return Ok(());
                }
                registry
                    .entries
                    .capacity()
                    .max(8)
                    .checked_mul(2)
                    .ok_or_else(|| std::io::Error::other("Cache registry overflow"))?
            };
            let mut charge = self.reserve_class(
                capacity * std::mem::size_of::<Weak<dyn Reclaimable>>(),
                ResourceCategory::Scratch,
            )?;
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(capacity)
                .map_err(std::io::Error::other)?;
            charge.commit(entries.capacity() * std::mem::size_of::<Weak<dyn Reclaimable>>());
            let mut registry = self.reclaimers.lock().unwrap();
            if registry.entries.len() >= capacity {
                drop(registry);
                continue;
            }
            entries.append(&mut registry.entries);
            entries.push(item);
            let old = std::mem::replace(
                &mut *registry,
                ReclaimerRegistry {
                    entries,
                    _charge: Some(charge.detach()),
                },
            );
            drop(registry);
            drop(old);
            return Ok(());
        }
    }
    pub fn prune_reclaimers(&self) {
        let Ok(mut registry) = self.reclaimers.try_lock() else {
            return;
        };
        registry.entries.retain(|e| e.strong_count() != 0);
        if registry.entries.is_empty() {
            let old = std::mem::take(&mut *registry);
            drop(registry);
            drop(old);
        }
    }
    fn reclaim_for(&self, additional: usize) {
        {
            let mut s = self.state.lock().unwrap();
            while s.stats.current.saturating_add(additional as u64) > self.limits.total as u64 {
                let Some((b, category)) = s.free.pop() else {
                    break;
                };
                s.stats.current -= b.len() as u64;
                s.stats.retained -= b.len() as u64;
                s.remove_live(category, b.len());
            }
            if s.stats.current.saturating_add(additional as u64) <= self.limits.total as u64 {
                return;
            }
        }
        let count = self.reclaimers.lock().unwrap().entries.len();
        let mut previous = 0;
        for _ in 0..count {
            let candidate = {
                let entries = self.reclaimers.lock().unwrap();
                entries
                    .entries
                    .iter()
                    .filter_map(Weak::upgrade)
                    .filter(|e| e.last_access() > previous)
                    .min_by_key(|e| e.last_access())
            };
            let Some(candidate) = candidate else {
                break;
            };
            previous = candidate.last_access();
            candidate.reclaim();
            // Evicted shared blocks may have been returned to the idle pool.
            let mut s = self.state.lock().unwrap();
            while s.stats.current.saturating_add(additional as u64) > self.limits.total as u64 {
                let Some((b, category)) = s.free.pop() else {
                    break;
                };
                s.stats.current -= b.len() as u64;
                s.stats.retained -= b.len() as u64;
                s.remove_live(category, b.len());
            }
            if s.stats.current.saturating_add(additional as u64) <= self.limits.total as u64 {
                break;
            }
        }
    }
    pub fn acquire_lease(&self) {
        self.leases
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    pub fn release_lease(&self) {
        self.leases
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        self.capacity_changed();
    }
    pub fn lease_count(&self) -> usize {
        self.leases.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn has_leases(&self) -> bool {
        self.leases.load(std::sync::atomic::Ordering::Acquire) != 0
    }
    pub fn statistics(&self) -> BudgetStatistics {
        self.state.lock().unwrap().stats
    }
    pub fn detailed_statistics(&self) -> DetailedStatistics {
        let state = self.state.lock().unwrap();
        let mut result = state.detailed;
        result.current_bytes = state.stats.current;
        result.peak_bytes = state.stats.peak;
        result.idle_bytes = state.stats.retained;
        result
    }
    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }
    pub fn copied(&self, n: usize) {
        self.copy_bytes(CopyKind::Other, n);
    }
    pub fn copy_bytes(&self, kind: CopyKind, n: usize) {
        let mut s = self.state.lock().unwrap();
        if !matches!(kind, CopyKind::Delivery) {
            s.stats.copied += n as u64;
        }
        let f = &mut s.detailed.flow;
        match kind {
            CopyKind::Input => f.input_copy += n as u64,
            CopyKind::Compaction => f.compaction_copy += n as u64,
            CopyKind::Delivery => f.delivery_copy += n as u64,
            CopyKind::Other => f.other_copy += n as u64,
        }
    }
    pub fn codec_bytes(&self, encode: bool, input: usize, output: usize) {
        let mut s = self.state.lock().unwrap();
        let f = &mut s.detailed.flow;
        if encode {
            f.encoded_input += input as u64;
            f.encoded_output += output as u64;
        } else {
            f.decoded_input += input as u64;
            f.decoded_output += output as u64;
        }
    }
    pub fn decode_event(&self, complete: bool) {
        let mut s = self.state.lock().unwrap();
        if complete {
            s.detailed.flow.decode_completed += 1;
        } else {
            s.detailed.flow.decode_started += 1;
        }
    }
    pub fn cache_event(&self, kind: u8) {
        let mut s = self.state.lock().unwrap();
        match kind {
            0 => s.detailed.flow.cache_misses += 1,
            1 => s.detailed.flow.cache_hits += 1,
            _ => s.detailed.flow.cache_evictions += 1,
        }
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> std::io::Result<Reservation> {
        self.reserve_class(bytes, ResourceCategory::Scratch)
    }
    pub fn reserve_class(
        self: &Arc<Self>,
        bytes: usize,
        category: ResourceCategory,
    ) -> std::io::Result<Reservation> {
        let mut r = Reservation {
            budget: self.clone(),
            bytes: 0,
            live: 0,
            category,
            references: ReferenceCounts::default(),
        };
        r.resize(bytes)?;
        Ok(r)
    }
    fn allocate(
        self: &Arc<Self>,
        size: usize,
        category: ResourceCategory,
    ) -> std::io::Result<Arc<Allocation>> {
        if size > self.limits.block {
            return Err(std::io::Error::other(StorageLimit {
                resource: "StorageBlock",
                limit: self.limits.block,
                requested: size,
                current: self.statistics().current as usize,
                phase: "storage allocation",
            }));
        }
        // Bounded size classes prevent retention of arbitrarily many tiny allocations.
        let size = size
            .max(4096)
            .checked_next_power_of_two()
            .unwrap_or(self.limits.block)
            .min(self.limits.block);
        // Try idle reuse before domain-wide pressure eviction.
        let reusable = self
            .state
            .lock()
            .unwrap()
            .free
            .iter()
            .any(|(b, _)| b.len() >= size);
        if !reusable {
            self.reclaim_for(size);
        }
        let mut s = self.state.lock().unwrap();
        if let Some(i) = s
            .free
            .iter()
            .enumerate()
            .filter(|(_, (b, _))| b.len() >= size)
            .min_by_key(|(_, (b, _))| b.len())
            .map(|(i, _)| i)
        {
            let (bytes, old_category) = s.free.swap_remove(i);
            s.stats.retained -= bytes.len() as u64;
            s.remove_live(old_category, bytes.len());
            s.add_live(category, bytes.len(), false);
            return Ok(Arc::new(Allocation::new(bytes, self.clone(), category)));
        }
        while s.stats.current.saturating_add(size as u64) > self.limits.total as u64
            && !s.free.is_empty()
        {
            let (b, category) = s.free.pop().unwrap();
            s.remove_live(category, b.len());
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
        s.add_live(category, bytes.len(), true);
        Ok(Arc::new(Allocation::new(bytes, self.clone(), category)))
    }
}
pub struct Reservation {
    references: ReferenceCounts,
    budget: Arc<MemoryBudget>,
    bytes: usize,
    live: usize,
    category: ResourceCategory,
}
impl Reservation {
    pub fn reference(&self, cache: bool, acquire: bool) {
        self.references
            .update(&self.budget, self.bytes, cache, acquire);
    }
    fn detach(mut self) -> DetachedReservation {
        let result = DetachedReservation {
            budget: Arc::downgrade(&self.budget),
            bytes: self.bytes,
            live: self.live,
            category: self.category,
        };
        self.bytes = 0;
        self.live = 0;
        result
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn limits(&self) -> BudgetLimits {
        self.budget.limits
    }
    pub fn domain(&self) -> Arc<MemoryBudget> {
        self.budget.clone()
    }
    pub fn commit(&mut self, live: usize) {
        assert!(live <= self.bytes);
        let mut s = self.budget.state.lock().unwrap();
        let c = &mut s.detailed.resources[self.category as usize];
        c.live = c.live - self.live as u64 + live as u64;
        c.reserved = c.current - c.live;
        if live > self.live {
            s.detailed.allocation_count += 1;
            s.detailed.allocated_bytes += (live - self.live) as u64;
        }
        self.live = live;
    }
    pub fn resize(&mut self, bytes: usize) -> std::io::Result<()> {
        if bytes > self.bytes {
            self.budget.reclaim_for(bytes - self.bytes);
        }
        let mut s = self.budget.state.lock().unwrap();
        if s.stats.current - self.bytes as u64 + bytes as u64 > self.budget.limits.total as u64 {
            while s.stats.current - self.bytes as u64 + bytes as u64
                > self.budget.limits.total as u64
                && !s.free.is_empty()
            {
                let (b, category) = s.free.pop().unwrap();
                s.remove_live(category, b.len());
                s.stats.current -= b.len() as u64;
                s.stats.retained -= b.len() as u64;
            }
        }
        if s.stats.current - self.bytes as u64 + bytes as u64 > self.budget.limits.total as u64 {
            s.detailed.rejected += 1;
            return Err(std::io::Error::new(
                if bytes > self.budget.limits.total {
                    std::io::ErrorKind::Other
                } else {
                    std::io::ErrorKind::WouldBlock
                },
                StorageLimit {
                    resource: match self.category {
                        ResourceCategory::CodecEncoder => "CodecEncoder",
                        ResourceCategory::CodecDecoder => "CodecDecoder",
                        _ => "NativeDomain",
                    },
                    limit: self.budget.limits.total,
                    requested: bytes.saturating_sub(self.bytes),
                    current: s.stats.current as usize,
                    phase: "reservation",
                },
            ));
        }
        s.stats.current = s.stats.current - self.bytes as u64 + bytes as u64;
        s.stats.peak = s.stats.peak.max(s.stats.current);
        if bytes > self.bytes {
            s.stats.allocations += 1;
        }
        let c = &mut s.detailed.resources[self.category as usize];
        c.current = c.current - self.bytes as u64 + bytes as u64;
        c.peak = c.peak.max(c.current);
        if self.live > bytes {
            c.live -= (self.live - bytes) as u64;
            self.live = bytes;
        }
        c.reserved = c.current - c.live;
        if bytes < self.bytes {
            self.budget.capacity_changed();
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut s = self.budget.state.lock().unwrap();
        s.stats.current -= self.bytes as u64;
        let c = &mut s.detailed.resources[self.category as usize];
        c.current -= self.bytes as u64;
        c.live -= self.live as u64;
        c.reserved = c.current - c.live;
        if self.bytes != 0 {
            self.budget.capacity_changed();
        }
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
        Ok(Self(Some(Arc::new(Mutex::new(
            budget.reserve_class(0, ResourceCategory::Declaration)?,
        )))))
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
}
struct Allocation {
    references: ReferenceCounts,
    category: ResourceCategory,
    pointer: *mut u8,
    length: usize,
    budget: Arc<MemoryBudget>,
}
// Only WriteBuffer mutates, and only outside every published SharedBytes range.
unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}
impl Allocation {
    fn new(bytes: Box<[u8]>, budget: Arc<MemoryBudget>, category: ResourceCategory) -> Self {
        let length = bytes.len();
        Self {
            category,
            references: ReferenceCounts::default(),
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
            s.free.push((bytes, self.category));
        } else {
            s.stats.current -= bytes.len() as u64;
            s.remove_live(self.category, bytes.len());
        }
        self.budget.capacity_changed();
    }
}
#[derive(Clone)]
enum Backing {
    Owned(Arc<Allocation>),
    External(Arc<dyn SharedSource>),
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
    pub fn lease_reference(&self, acquire: bool) {
        SharedSource::reference(self, false, acquire);
    }
    pub fn cache_reference(&self, acquire: bool) {
        SharedSource::reference(self, true, acquire);
    }
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
    pub fn external(source: Arc<dyn SharedSource>, range: Range<usize>) -> Self {
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
impl SharedSource for SharedBytes {
    fn reference(&self, cache: bool, acquire: bool) {
        match &self.backing {
            Backing::Owned(b) => b.references.update(&b.budget, b.length, cache, acquire),
            Backing::External(b) => b.reference(cache, acquire),
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
    pub category: ResourceCategory,
}
impl Default for WriteBuffer {
    fn default() -> Self {
        Self {
            backing: None,
            budget: Arc::new(MemoryBudget::default()),
            category: ResourceCategory::Input,
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
        let allocation = self.budget.allocate(size, self.category)?;
        let old = &self.bytes()[preserve.clone()];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copy_bytes(CopyKind::Compaction, old.len());
        self.backing = Some(Backing::Owned(allocation));
        Ok(())
    }
    pub fn relocate(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        let allocation = self.budget.allocate(size, self.category)?;
        let old = &self.bytes()[preserve];
        unsafe {
            allocation.tail(0..old.len()).copy_from_slice(old);
        }
        self.budget.copy_bytes(CopyKind::Compaction, old.len());
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

#[cfg(test)]
mod capacity_version_tests {
    use super::*;
    #[test]
    fn detects_release_even_when_capacity_is_immediately_reoccupied() {
        let domain = Arc::new(MemoryBudget::default());
        let first = domain
            .reserve_class(1024, ResourceCategory::Scratch)
            .unwrap();
        let version = domain.capacity_version();
        drop(first);
        let _second = domain
            .reserve_class(1024, ResourceCategory::Scratch)
            .unwrap();
        assert_eq!(domain.statistics().current, 1024);
        assert!(domain.capacity_version() > version);
        let version = domain.capacity_version();
        drop(domain.reserve_class(0, ResourceCategory::Scratch).unwrap());
        assert_eq!(domain.capacity_version(), version);
    }
}
