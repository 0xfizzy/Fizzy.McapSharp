//! Complete, immutable chunks with a byte-limited LRU. Storage owners survive eviction.
use super::*;
use mcap::charged::{weak::BudgetedArc, ChargedShared};
use std::io::Cursor;
use std::mem::ManuallyDrop;
use std::sync::{
    atomic::{AtomicU64, Ordering}, Mutex,
};
pub(super) trait CacheContent: Send + Sync {
    fn operation_reference(&self, acquire: bool);
    fn cache_reference(&self, acquire: bool);
}
/// The operation pin is acquired under the cache lock before publishing access.
/// Eviction can remove the cache reference without making this storage reclaimable.
pub(super) struct CachePin<T: CacheContent>(ManuallyDrop<ChargedShared<T>>);
impl<T: CacheContent> CachePin<T> {
    fn new(value: ChargedShared<T>) -> Self {
        value.charge_owner(mcap::storage::OwnerKind::Operation, true);
        value.operation_reference(true);
        Self(ManuallyDrop::new(value))
    }
}
impl<T: CacheContent> std::ops::Deref for CachePin<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: CacheContent> Drop for CachePin<T> {
    fn drop(&mut self) {
        let _release = self.0.begin_capacity_release();
        let value = unsafe { ManuallyDrop::take(&mut self.0) };
        value.operation_reference(false);
        value.charge_owner(mcap::storage::OwnerKind::Operation, false);
        drop(value);
    }
}
struct CacheItem<T: CacheContent> {
    value: Mutex<Option<ChargedShared<T>>>,
    cost: u64,
    touched: AtomicU64,
    domain: mcap::storage::BudgetRef,
}
impl<T: CacheContent + 'static> CacheItem<T> {
    fn new(
        value: ChargedShared<T>,
        cost: u64,
        domain: &mcap::storage::BudgetRef,
    ) -> Outcome<BudgetedArc<Self>> {
        value.charge_owner(mcap::storage::OwnerKind::Cache, true);
        value.cache_reference(true);
        let item = BudgetedArc::new_with_owner_fixed(
            Self {
                value: Mutex::new(Some(value)),
                cost,
                touched: AtomicU64::new(domain.touch()),
                domain: domain.clone(),
            },
            domain,
            mcap::storage::ResourceCategory::Scratch,
            // The reader retains this mutable directory shell even after payload eviction.
            // It is not immediately reclaimable merely because its content is cache-only.
            mcap::storage::OwnerKind::Parser,
        )?;
        domain.register_reclaimer_fixed(item.reclaimer())?;
        Ok(item)
    }
    fn get(&self) -> Option<CachePin<T>> {
        self.value
            .lock()
            .unwrap()
            .as_ref()
            .map(|value| CachePin::new(value.clone()))
    }
}
impl<T: CacheContent> Drop for CacheItem<T> {
    fn drop(&mut self) {
        let _release = self.domain.begin_capacity_release();
        if let Some(value) = self.value.get_mut().unwrap().take() {
            value.cache_reference(false);
            value.charge_owner(mcap::storage::OwnerKind::Cache, false);
        }
        self.domain.prune_reclaimers();
    }
}
impl<T: CacheContent> mcap::storage::Reclaimable for CacheItem<T> {
    fn last_access(&self) -> u64 {
        self.touched.load(Ordering::Relaxed)
    }
    fn reclaim(&self) -> bool {
        let old = match self.value.try_lock() {
            Ok(mut value) => value.take(),
            Err(_) => return false,
        };
        let removed = old.is_some();
        // Keep admission open between removing ownership and freeing storage.
        // The cache mutex is already released; destruction must stay outside it.
        let _release = removed.then(|| self.domain.begin_capacity_release());
        if let Some(value) = &old {
            value.cache_reference(false);
            value.charge_owner(mcap::storage::OwnerKind::Cache, false);
        }
        drop(old);
        if removed {
            self.domain.cache_event(2);
        }
        removed
    }
}
pub(super) struct CacheKey {
    // Fields drop in order: storage must be freed before its reservation.
    bytes: Vec<u8>,
    charge: mcap::storage::Reservation,
    pending: bool,
}
impl CacheKey {
    pub(super) fn new(domain: &mcap::storage::BudgetRef, key: &[u8]) -> Outcome<Self> {
        let (mut bytes, charge) =
            mcap::charged::vector_fixed(domain, mcap::storage::ResourceCategory::Scratch, key.len())?;
        bytes.extend_from_slice(key);
        domain.copy_bytes(mcap::storage::CopyKind::Other, key.len());
        Ok(Self {
            bytes,
            charge,
            pending: false,
        })
    }
}
impl Drop for CacheKey {
    fn drop(&mut self) {
        if self.pending {
            self.charge
                .owner_reference(mcap::storage::OwnerKind::Pending, false);
        }
    }
}
struct Entry {
    offset: u64,
    header: MessageHeader,
    data: mcap::storage::SharedBytes,
}
pub(super) struct Chunk {
    key: CacheKey,
    entries: mcap::segmented::BudgetedSegmentedVec<Entry>,
}
impl Chunk {
    pub fn message(&self, summary: &mcap::Summary, offset: u64) -> Outcome<lease::Message> {
        let i = self
            .entries
            .binary_search_by_key(&offset, |e| e.offset)
            .map_err(|_| mcap::McapError::BadIndex)?;
        let e = &self.entries[i];
        if !summary.channels.contains_key(&e.header.channel_id) {
            return Err(
                mcap::McapError::UnknownChannel(e.header.sequence, e.header.channel_id).into(),
            );
        }
        Ok(lease::Message {
            header: e.header,
            data: e.data.clone_for(mcap::storage::OwnerKind::Operation),
        })
    }
}
pub struct PackedIndex {
    key: CacheKey,
    pub data: mcap::segmented::BudgetedSegmentedVec<[u8; 18]>,
}
impl CacheContent for Chunk {
    fn operation_reference(&self, acquire: bool) {
        use mcap::storage::{OwnerKind, SharedSource};
        self.entries.owner_reference(OwnerKind::Operation, acquire);
        self.key
            .charge
            .owner_reference(OwnerKind::Operation, acquire);
        for e in self.entries.iter() {
            e.data.owner_reference(OwnerKind::Operation, acquire);
        }
    }
    fn cache_reference(&self, acquire: bool) {
        self.entries
            .owner_reference(mcap::storage::OwnerKind::Cache, acquire);
        self.key
            .charge
            .owner_reference(mcap::storage::OwnerKind::Cache, acquire);
        for entry in self.entries.iter() {
            entry.data.cache_reference(acquire);
        }
    }
}
impl CacheContent for PackedIndex {
    fn operation_reference(&self, acquire: bool) {
        self.data
            .owner_reference(mcap::storage::OwnerKind::Operation, acquire);
        self.key
            .charge
            .owner_reference(mcap::storage::OwnerKind::Operation, acquire);
    }
    fn cache_reference(&self, acquire: bool) {
        self.data
            .owner_reference(mcap::storage::OwnerKind::Cache, acquire);
        self.key
            .charge
            .owner_reference(mcap::storage::OwnerKind::Cache, acquire);
    }
}
impl PackedIndex {
    pub unsafe fn copy_to(
        &self,
        dest: *mut u8,
        capacity: usize,
        out: &mut Response,
    ) -> Outcome<i32> {
        let length = self
            .data
            .len()
            .checked_mul(18)
            .ok_or("Index capacity overflow")?;
        out.value = length as u64;
        if capacity < length {
            return Ok(2);
        }
        if dest.is_null() && length != 0 {
            return Err("Null destination".into());
        }
        for (i, row) in self.data.iter().enumerate() {
            memory::copy(row, dest.add(i * 18))?;
        }
        Ok(0)
    }
}
struct Pending {
    key: CacheKey,
    offset: u64,
    message: lease::Message,
}
pub struct ChunkCache {
    pending: Option<Pending>,
    indexes: mcap::segmented::BudgetedSegmentedVec<BudgetedArc<CacheItem<PackedIndex>>>,
    chunks: mcap::segmented::BudgetedSegmentedVec<BudgetedArc<CacheItem<Chunk>>>,
    domain: mcap::storage::BudgetRef,
    pub stats: memory::Statistics,
    pub hits: u64,
    pub loads: u64,
}
impl Default for ChunkCache {
    fn default() -> Self {
        Self::new(Default::default())
    }
}
impl ChunkCache {
    pub fn new(domain: mcap::storage::BudgetRef) -> Self {
        Self {
            indexes: mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
                domain.clone(),
                mcap::storage::ResourceCategory::Scratch,
                64,
            ),
            pending: None,
            chunks: mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
                domain.clone(),
                mcap::storage::ResourceCategory::Scratch,
                64,
            ),
            domain,
            stats: Default::default(),
            hits: 0,
            loads: 0,
        }
    }
    pub fn clear(&mut self) {
        self.pending = None;
        self.indexes.clear();
        self.chunks.clear();
        self.stats.current = 0;
    }
    pub fn resident_bytes(&self) -> u64 {
        self.chunks
            .iter()
            .filter(|c| c.value.lock().unwrap().is_some())
            .map(|c| c.cost)
            .sum::<u64>()
            + self
                .indexes
                .iter()
                .filter(|c| c.value.lock().unwrap().is_some())
                .map(|c| c.cost)
                .sum::<u64>()
    }
    fn refresh(&mut self) {
        self.chunks.retain(|c| c.value.lock().unwrap().is_some());
        self.indexes.retain(|c| c.value.lock().unwrap().is_some());
        self.stats.current = self.chunks.iter().map(|c| c.cost).sum::<u64>()
            + self.indexes.iter().map(|c| c.cost).sum::<u64>();
    }
    fn evict_one(&mut self) -> bool {
        if !self.chunks.is_empty() {
            let old = self.chunks.remove(0);
            self.stats.current = self.stats.current.saturating_sub(old.cost);
            true
        } else if !self.indexes.is_empty() {
            let old = self.indexes.remove(0);
            self.stats.current = self.stats.current.saturating_sub(old.cost);
            true
        } else {
            false
        }
    }
    fn reserve(&mut self, n: usize) -> Outcome<mcap::storage::Reservation> {
        loop {
            match self.domain.reserve(n) {
                Ok(charge) => return Ok(charge),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && self.evict_one() => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn copy_key(&mut self, key: &[u8]) -> Outcome<CacheKey> {
        loop {
            match CacheKey::new(&self.domain, key) {
                Ok(r) => return Ok(r),
                Err(e) if budget::unavailable(&e) && self.evict_one() => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn message_indexes(
        &mut self,
        input: &[u8],
        summary: &mcap::Summary,
        index: &mcap::shared_chunk_index::SharedChunkIndex,
        key: &[u8],
        limit: u64,
    ) -> Outcome<CachePin<PackedIndex>> {
        self.refresh();
        for slot in &self.indexes {
            if let Some(cached) = slot.get() {
                if cached.key.bytes == key {
                    slot.touched.store(self.domain.touch(), Ordering::Relaxed);
                    return Ok(cached);
                }
            }
        }
        if index.message_index_offsets.is_empty() {
            return Err(mcap::McapError::BadIndex.into());
        }
        let owned_key = self.copy_key(key)?;
        let mut packed = mcap::segmented::BudgetedSegmentedVec::new(
            self.domain.clone(),
            mcap::storage::ResourceCategory::Index,
        );
        for (channel_id, offset) in index.message_index_offsets.iter() {
            let body = extended::record_body(input, *offset, records::op::MESSAGE_INDEX)?;
            let mut cursor = Cursor::new(body);
            let actual: u16 =
                binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
            if actual != channel_id {
                return Err(mcap::McapError::BadIndex.into());
            }
            if !summary.channels.contains_key(&channel_id) {
                return Err(mcap::McapError::UnknownChannel(0, channel_id).into());
            }
            // Same byte-length termination as the official records::parse_vec; read each
            // official entry directly into its final descriptor page.
            let byte_len: u32 =
                binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
            let start = cursor.position();
            while cursor.position() - start < byte_len as u64 {
                let entry: records::MessageIndexEntry =
                    binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
                let mut row = [0u8; 18];
                row[..2].copy_from_slice(&channel_id.to_le_bytes());
                row[2..10].copy_from_slice(&entry.log_time.to_le_bytes());
                row[10..].copy_from_slice(&entry.offset.to_le_bytes());
                packed.push_fixed(row)?;
            }
        }
        let cost = key
            .len()
            .checked_add(packed.allocated_bytes())
            .ok_or("Index capacity overflow")?;
        let cached = CachePin::new(ChargedShared::new_fixed(
            PackedIndex {
                key: owned_key,
                data: packed,
            },
            &self.domain,
            mcap::storage::ResourceCategory::Scratch,
        )?);
        let cost = cost
            .checked_add(cached.0.allocation_bytes())
            .and_then(|cost| {
                cost.checked_add(BudgetedArc::<CacheItem<PackedIndex>>::allocation_size())
            })
            .ok_or("Index capacity overflow")?;
        if cost as u64 <= limit && limit != 0 {
            while self.stats.current.saturating_add(cost as u64) > limit {
                if !self.indexes.is_empty() {
                    let old = self.indexes.remove(0);
                    self.stats.current = self.stats.current.saturating_sub(old.cost);
                } else if !self.chunks.is_empty() {
                    let old = self.chunks.remove(0);
                    self.stats.current = self.stats.current.saturating_sub(old.cost);
                } else {
                    break;
                }
            }
            self.stats.capacity(
                self.stats.current as usize,
                self.stats.current as usize + cost,
            );
            self.indexes
                .push_fixed(CacheItem::new(ChargedShared::clone(&cached.0), cost as u64, &self.domain)?)?;
        }
        Ok(cached)
    }
    pub fn load(
        &mut self,
        input: &memory::Source,
        index: &mcap::shared_chunk_index::SharedChunkIndex,
        key: &[u8],
        limit: u64,
    ) -> Outcome<CachePin<Chunk>> {
        self.refresh();
        for slot in &self.chunks {
            if let Some(chunk) = slot.get() {
                if chunk.key.bytes == key {
                    self.hits += 1;
                    self.domain.cache_event(1);
                    slot.touched.store(self.domain.touch(), Ordering::Relaxed);
                    return Ok(chunk);
                }
            }
        }
        self.domain.cache_event(0);
        let start = usize::try_from(
            index
                .chunk_start_offset
                .checked_add(9)
                .ok_or(mcap::McapError::BadIndex)?,
        )?;
        let end = usize::try_from(
            index
                .chunk_start_offset
                .checked_add(index.chunk_length)
                .ok_or(mcap::McapError::BadIndex)?,
        )?;
        let body = input.get(start..end).ok_or(mcap::McapError::BadIndex)?;
        let (header, data) = mcap::read::parse_borrowed_chunk(body)?;
        let uncompressed = header.uncompressed_size;
        // Evict before loading. Active leases keep storage charged in the resource domain.
        while !self.chunks.is_empty()
            && self
                .stats
                .current
                .saturating_add(uncompressed)
                .saturating_add(key.len() as u64)
                > limit
        {
            let c = self.chunks.remove(0);
            self.stats.current = self.stats.current.saturating_sub(c.cost);
        }
        while !self.chunks.is_empty()
            && self
                .domain
                .statistics()
                .current
                .saturating_add(uncompressed)
                > self.domain.limits().total as u64
        {
            let c = self.chunks.remove(0);
            self.stats.current = self.stats.current.saturating_sub(c.cost);
        }
        drop(self.reserve(self.domain.limits().block.min(4096))?);
        let mut parser =
            buffer_reader::chunk_parser(header, &data, body.len(), self.domain.clone())?;
        parser.set_memory_budget(self.domain.clone())?;
        let mut fed = false;
        let mut entries = mcap::segmented::BudgetedSegmentedVec::<Entry>::new(
            self.domain.clone(),
            mcap::storage::ResourceCategory::Index,
        );
        let owned_key = self.copy_key(key)?;
        let mut position = 0u64;
        loop {
            let event = match parser.next_shared_event().transpose() {
                Err(mcap::McapError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock && self.evict_one() =>
                {
                    continue
                }
                other => other?,
            };
            match event {
                None => break,
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(_)) => {
                    if fed {
                        parser.notify_read(0);
                    } else {
                        parser.supply_shared(input.shared(start..end));
                        fed = true;
                    }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data }) => {
                    record_input::validate(opcode,data.as_ref(),&self.domain)?;
                    if opcode == records::op::MESSAGE {
                        let records::Record::Message {header,..} = mcap::parse_record(opcode,data.as_ref())? else {unreachable!()};
                        entries.push_fixed(Entry {
                            offset: position,
                            header: buffer_reader::native_header(&header),
                            data: data
                                .slice(22..data.as_ref().len())
                                .clone_for(mcap::storage::OwnerKind::Cache),
                        })?;
                    }
                    position = position
                        .checked_add(9 + data.as_ref().len() as u64)
                        .ok_or(mcap::McapError::BadIndex)?;
                }
            }
        }
        self.loads += 1;
        let storage_capacity = entries
            .iter()
            .filter_map(|e| e.data.owned_capacity())
            .max()
            .map(|n| n as u64)
            .unwrap_or(uncompressed);
        let cost = storage_capacity.saturating_add((key.len() + entries.allocated_bytes()) as u64);
        let chunk = CachePin::new(ChargedShared::new_fixed(
            Chunk {
                key: owned_key,
                entries,
            },
            &self.domain,
            mcap::storage::ResourceCategory::Scratch,
        )?);
        let cost = cost
            .saturating_add(chunk.0.allocation_bytes() as u64)
            .saturating_add(BudgetedArc::<CacheItem<Chunk>>::allocation_size() as u64);
        if limit != 0 && cost <= limit {
            while self.stats.current.saturating_add(cost) > limit {
                if !self.chunks.is_empty() {
                    let c = self.chunks.remove(0);
                    self.stats.current = self.stats.current.saturating_sub(c.cost);
                } else {
                    let c = self.indexes.remove(0);
                    self.stats.current = self.stats.current.saturating_sub(c.cost);
                }
            }
            self.stats.capacity(
                self.stats.current as usize,
                (self.stats.current + cost) as usize,
            );
            self.chunks
                .push_fixed(CacheItem::new(ChargedShared::clone(&chunk.0), cost, &self.domain)?)?;
        }
        Ok(chunk)
    }
    pub fn message(
        &mut self,
        input: &memory::Source,
        summary: &mcap::Summary,
        index: &mcap::shared_chunk_index::SharedChunkIndex,
        key: &[u8],
        offset: u64,
        limit: u64,
    ) -> Outcome<lease::Message> {
        let chunk = self.load(input, index, key, limit)?;
        chunk.message(summary, offset)
    }
    pub unsafe fn read(
        &mut self,
        input: &memory::Source,
        summary: &mcap::Summary,
        index: &mcap::shared_chunk_index::SharedChunkIndex,
        key: &[u8],
        offset: u64,
        limit: u64,
        dest: *mut u8,
        capacity: usize,
        header: *mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<Option<i32>> {
        self.read_with(
            input, summary, index, key, offset, limit, dest, capacity, header, out, None,
        )
    }
    pub unsafe fn read_with(
        &mut self,
        input: &memory::Source,
        summary: &mcap::Summary,
        index: &mcap::shared_chunk_index::SharedChunkIndex,
        key: &[u8],
        offset: u64,
        limit: u64,
        dest: *mut u8,
        capacity: usize,
        header: *mut MessageHeader,
        out: &mut Response,
        sink: Option<memory::Sink>,
    ) -> Outcome<Option<i32>> {
        let message = if self
            .pending
            .as_ref()
            .is_some_and(|p| p.key.bytes == key && p.offset == offset)
        {
            self.pending.take().unwrap().message
        } else {
            self.pending = None;
            self.message(input, summary, index, key, offset, limit)?
        };
        if !header.is_null() {
            *header = message.header;
        }
        let data = message.data.as_ref();
        out.value = data.len() as u64;
        if let Some(sink) = sink {
            let copied = sink.send(records::op::MESSAGE, &message.header, data)?;
            self.stats.copied += copied;
            self.domain
                .copy_bytes(mcap::storage::CopyKind::Delivery, copied as usize);
        } else {
            if capacity < data.len() {
                let mut owned_key = self.copy_key(key)?;
                let mut message = message;
                message.data.set_owner(mcap::storage::OwnerKind::Pending);
                owned_key
                    .charge
                    .owner_reference(mcap::storage::OwnerKind::Pending, true);
                owned_key.pending = true;
                self.pending = Some(Pending {
                    key: owned_key,
                    offset,
                    message,
                });
                return Ok(Some(2));
            }
            memory::copy(data, dest)?;
            self.stats.copied += data.len() as u64;
            self.domain
                .copy_bytes(mcap::storage::CopyKind::Delivery, data.len());
        }
        Ok(Some(0))
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use mcap::storage::{Reclaimable, ResourceCategory};
    #[test]
    fn operation_pin_release_covers_payload_and_control_cleanup() {
        use mcap::storage::{CapacityWaitStatus, OwnerKind};
        struct Content(mcap::storage::Reservation);
        impl CacheContent for Content {
            fn operation_reference(&self, acquire: bool) {
                self.0.owner_reference(OwnerKind::Operation, acquire);
                if !acquire {
                    assert_eq!(self.0.domain().retry_status(4096).unwrap(), CapacityWaitStatus::Waiting);
                }
            }
            fn cache_reference(&self, _: bool) {}
        }
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            total: (65536) + mcap::storage::BudgetRef::allocation_size(), block: 65536, retained: 0,
        }).unwrap();
        let pin = CachePin::new(ChargedShared::new_fixed(Content(domain.reserve(4096).unwrap()),
            &domain, ResourceCategory::Scratch).unwrap());
        let fixed = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
        drop(pin);
        assert_eq!(domain.retry_status(4096).unwrap(), CapacityWaitStatus::Ready);
        drop(fixed);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn cache_release_admission_covers_owner_removal_until_deallocation() {
        use mcap::storage::{CapacityWaitStatus, CapacityWaitTicket, OwnerKind};
        struct Content {
            charge: mcap::storage::Reservation,
            requested: usize,
        }
        impl CacheContent for Content {
            fn operation_reference(&self, _: bool) {}
            fn cache_reference(&self, acquire: bool) {
                self.charge.owner_reference(OwnerKind::Cache, acquire);
                if !acquire {
                    // Deterministically observe the window before physical release.
                    assert_eq!(self.charge.domain().retry_status(self.requested).unwrap(),
                        CapacityWaitStatus::Waiting);
                }
            }
        }
        for reclaim in [false, true] {
            let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
                total: (65536) + mcap::storage::BudgetRef::allocation_size(), block: 65536, retained: 0,
            }).unwrap();
            let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
            let value = ChargedShared::new_fixed(Content {
                charge: domain.reserve(8192).unwrap(), requested: 4096,
            }, &domain, ResourceCategory::Scratch).unwrap();
            let cached = CacheItem::new(value, 8192, &domain).unwrap();
            let blocker = domain.reserve(65536 - domain.workload_statistics().current as usize).unwrap();
            if reclaim { assert!(cached.reclaim()); }
            drop(cached);
            assert_eq!(ticket.arm_for_external_release(4096).unwrap(), CapacityWaitStatus::Ready);
            drop(ticket);
            drop(blocker);
            domain.prune_reclaimers();
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
    #[test]
    fn failed_control_or_registry_allocation_restores_cache_ownership() {
        for allow_control in [false, true] {
            let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
                total: (16384) + mcap::storage::BudgetRef::allocation_size(),
                block: 16384,
                retained: 0,
            }).unwrap();
            let pin = CachePin::new(
                ChargedShared::new_fixed(
                    PackedIndex {
                        key: CacheKey::new(&domain, &[]).unwrap(),
                        data: mcap::segmented::BudgetedSegmentedVec::new(
                            domain.clone(),
                            ResourceCategory::Index,
                        ),
                    },
                    &domain,
                    ResourceCategory::Scratch,
                )
                .unwrap(),
            );
            let remaining = 16384 - domain.workload_statistics().current as usize;
            let control = if allow_control {
                BudgetedArc::<CacheItem<PackedIndex>>::allocation_size()
            } else {
                0
            };
            let blocker = domain
                .reserve_class(remaining - control, ResourceCategory::Scratch)
                .unwrap();
            let before = domain.workload_statistics().current;
            assert!(CacheItem::new(ChargedShared::clone(&pin.0), 0, &domain).is_err());
            assert_eq!(domain.workload_statistics().current, before);
            assert_eq!(
                domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Cache as usize],
                0
            );
            assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
            drop(blocker);
            drop(pin);
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }
    #[test]
    fn operation_pin_blocks_reclaimability_across_cache_eviction() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut data =
            mcap::segmented::BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Index);
        data.push([42; 18]).unwrap();
        let bytes = data.allocated_bytes() as u64;
        let pin = CachePin::new(
            ChargedShared::new_fixed(
                PackedIndex {
                    key: CacheKey::new(&domain, &[]).unwrap(),
                    data,
                },
                &domain,
                ResourceCategory::Scratch,
            )
            .unwrap(),
        );
        let bytes = bytes + pin.0.allocation_bytes() as u64;
        let cached = CacheItem::new(ChargedShared::clone(&pin.0), bytes, &domain).unwrap();
        let control_bytes = cached.allocation_bytes() as u64;
        assert_eq!(
            domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],
            control_bytes
        );
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        drop(pin);
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, bytes);
        let pin = cached.get().unwrap();
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        assert!(cached.reclaim());
        assert_eq!(
            domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],
            control_bytes
        );
        assert!(cached.get().is_none());
        assert_eq!(pin.data[0], [42; 18]);
        assert_eq!(domain.ownership_statistics().immediately_reclaimable, 0);
        drop(pin);
        drop(cached);
        domain.prune_reclaimers();
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

#[cfg(test)]
mod key_accounting_tests {
    use super::*;
    #[test]
    fn key_storage_is_exact_and_refusal_preserves_domain() {
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            total: (8) + mcap::storage::BudgetRef::allocation_size(),
            block: 8,
            retained: 0,
        }).unwrap();
        let key = CacheKey::new(&domain, b"12345678").unwrap();
        assert_eq!(key.bytes.capacity(), 8);
        let stats = domain.workload_detailed_statistics();
        assert_eq!(
            stats.resources[mcap::storage::ResourceCategory::Scratch as usize].live,
            8
        );
        assert_eq!(stats.allocation_count, 1);
        assert_eq!(stats.flow.other_copy, 8);
        assert!(CacheKey::new(&domain, b"x").is_err());
        assert_eq!(domain.workload_statistics().current, 8);
        assert_eq!(key.bytes, b"12345678");
        drop(key);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
