//! Complete, immutable chunks with a byte-limited LRU. Storage owners survive eviction.
use super::*;
use std::io::Cursor;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
trait CacheContent: Send + Sync {
    fn cache_reference(&self, acquire: bool);
}
struct CacheItem<T: CacheContent> {
    value: Mutex<Option<Arc<T>>>,
    cost: u64,
    touched: AtomicU64,
    domain: Arc<budget::MemoryBudget>,
    _charge: mcap::storage::Reservation,
}
impl<T: CacheContent + 'static> CacheItem<T> {
    fn new(value: Arc<T>, cost: u64, domain: &Arc<budget::MemoryBudget>) -> Outcome<Arc<Self>> {
        let mut charge = domain.reserve_class(
            std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>(),
            mcap::storage::ResourceCategory::Scratch,
        )?;
        charge.commit(charge.bytes());
        value.cache_reference(true);
        let item = Arc::new(Self {
            value: Mutex::new(Some(value)),
            cost,
            touched: AtomicU64::new(domain.touch()),
            domain: domain.clone(),
            _charge: charge,
        });
        let erased: Arc<dyn mcap::storage::Reclaimable> = item.clone();
        domain.register_reclaimer(Arc::downgrade(&erased))?;
        Ok(item)
    }
    fn get(&self) -> Option<Arc<T>> {
        self.value.lock().unwrap().clone()
    }
}
impl<T: CacheContent> Drop for CacheItem<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.get_mut().unwrap().take() {
            value.cache_reference(false);
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
        if let Some(value) = &old {
            value.cache_reference(false);
        }
        drop(old);
        if removed {
            self.domain.cache_event(2);
        }
        removed
    }
}
struct Entry {
    offset: u64,
    header: MessageHeader,
    data: mcap::storage::SharedBytes,
}
pub(super) struct Chunk {
    key: Vec<u8>,
    entries: mcap::segmented::BudgetedSegmentedVec<Entry>,
    _charge: mcap::storage::Reservation,
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
            data: e.data.clone(),
        })
    }
}
pub struct PackedIndex {
    key: Vec<u8>,
    pub data: mcap::segmented::BudgetedSegmentedVec<[u8; 18]>,
    _charge: mcap::storage::Reservation,
}
impl CacheContent for Chunk {
    fn cache_reference(&self, acquire: bool) {
        for entry in self.entries.iter() {
            entry.data.cache_reference(acquire);
        }
    }
}
impl CacheContent for PackedIndex {
    fn cache_reference(&self, _acquire: bool) {}
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
    key: Vec<u8>,
    offset: u64,
    message: lease::Message,
    _charge: mcap::storage::Reservation,
}
pub struct ChunkCache {
    pending: Option<Pending>,
    indexes: Vec<Arc<CacheItem<PackedIndex>>>,
    chunks: Vec<Arc<CacheItem<Chunk>>>,
    domain: Arc<budget::MemoryBudget>,
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
    pub fn new(domain: Arc<budget::MemoryBudget>) -> Self {
        Self {
            indexes: Vec::new(),
            pending: None,
            chunks: Vec::new(),
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
                Ok(r) => return Ok(r),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && self.evict_one() => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn message_indexes(
        &mut self,
        input: &[u8],
        summary: &mcap::Summary,
        index: &records::ChunkIndex,
        key: &[u8],
        limit: u64,
    ) -> Outcome<Arc<PackedIndex>> {
        self.refresh();
        for slot in &self.indexes {
            if let Some(cached) = slot.get() {
                if cached.key == key {
                    slot.touched.store(self.domain.touch(), Ordering::Relaxed);
                    return Ok(cached);
                }
            }
        }
        if index.message_index_offsets.is_empty() {
            return Err(mcap::McapError::BadIndex.into());
        }
        let mut charge = self.reserve(key.len())?;
        let mut packed = mcap::segmented::BudgetedSegmentedVec::new(
            self.domain.clone(),
            mcap::storage::ResourceCategory::Index,
        );
        for (channel_id, offset) in &index.message_index_offsets {
            let body = extended::record_body(input, *offset, records::op::MESSAGE_INDEX)?;
            let mut cursor = Cursor::new(body);
            let actual: u16 = binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
            if actual != *channel_id {
                return Err(mcap::McapError::BadIndex.into());
            }
            if !summary.channels.contains_key(channel_id) {
                return Err(mcap::McapError::UnknownChannel(0, *channel_id).into());
            }
            // Same byte-length termination as the official records::parse_vec; read each
            // official entry directly into its final descriptor page.
            let byte_len: u32 = binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
            let start = cursor.position();
            while cursor.position() - start < byte_len as u64 {
                let entry: records::MessageIndexEntry = binrw::BinReaderExt::read_le(&mut cursor).map_err(mcap::McapError::from)?;
                let mut row = [0u8; 18];
                row[..2].copy_from_slice(&channel_id.to_le_bytes());
                row[2..10].copy_from_slice(&entry.log_time.to_le_bytes());
                row[10..].copy_from_slice(&entry.offset.to_le_bytes());
                packed.push(row)?;
            }
        }
        let cost = key
            .len()
            .checked_add(packed.allocated_bytes())
            .ok_or("Index capacity overflow")?;
        charge.commit(key.len());
        let cached = Arc::new(PackedIndex {
            key: key.to_vec(),
            data: packed,
            _charge: charge,
        });
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
                .push(CacheItem::new(cached.clone(), cost as u64, &self.domain)?);
        }
        Ok(cached)
    }
    pub fn load(
        &mut self,
        input: &Arc<memory::Backing>,
        index: &records::ChunkIndex,
        key: &[u8],
        limit: u64,
    ) -> Outcome<Arc<Chunk>> {
        self.refresh();
        for slot in &self.chunks {
            if let Some(chunk) = slot.get() {
                if chunk.key == key {
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
        let records::Record::Chunk { header, data } = mcap::parse_record(records::op::CHUNK, body)?
        else {
            unreachable!()
        };
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
        parser.set_memory_budget(self.domain.clone());
        let mut fed = false;
        let mut entries = mcap::segmented::BudgetedSegmentedVec::<Entry>::new(
            self.domain.clone(),
            mcap::storage::ResourceCategory::Index,
        );
        let charge = self.reserve(key.len())?;
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
                        parser.supply_shared(mcap::storage::SharedBytes::external(
                            input.clone(),
                            start..end,
                        ));
                        fed = true;
                    }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data }) => {
                    let record = mcap::parse_record(opcode, data.as_ref())?;
                    if let records::Record::Message { header, .. } = record {
                        entries.push(Entry {
                            offset: position,
                            header: buffer_reader::native_header(&header),
                            data: data.slice(22..data.as_ref().len()),
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
        let chunk = Arc::new(Chunk {
            key: key.to_vec(),
            entries,
            _charge: charge,
        });
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
                .push(CacheItem::new(chunk.clone(), cost, &self.domain)?);
        }
        Ok(chunk)
    }
    pub fn message(
        &mut self,
        input: &Arc<memory::Backing>,
        summary: &mcap::Summary,
        index: &records::ChunkIndex,
        key: &[u8],
        offset: u64,
        limit: u64,
    ) -> Outcome<lease::Message> {
        let chunk = self.load(input, index, key, limit)?;
        chunk.message(summary, offset)
    }
    pub unsafe fn read(
        &mut self,
        input: &Arc<memory::Backing>,
        summary: &mcap::Summary,
        index: &records::ChunkIndex,
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
        input: &Arc<memory::Backing>,
        summary: &mcap::Summary,
        index: &records::ChunkIndex,
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
            .is_some_and(|p| p.key == key && p.offset == offset)
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
                let charge = self.domain.reserve(key.len())?;
                self.pending = Some(Pending {
                    key: key.to_vec(),
                    offset,
                    message,
                    _charge: charge,
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
