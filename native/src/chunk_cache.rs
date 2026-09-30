//! Complete, immutable chunks with a byte-limited LRU. Storage owners survive eviction.
use super::*;
use std::sync::Arc;
struct Entry {
    offset: u64,
    header: MessageHeader,
    data: mcap::storage::SharedBytes,
}
pub(super) struct Chunk {
    key: Vec<u8>,
    entries: Vec<Entry>,
    cost: u64,
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
    pub data: Vec<u8>,
    _charge: mcap::storage::Reservation,
}
struct Pending {
    key: Vec<u8>,
    offset: u64,
    message: lease::Message,
    _charge: mcap::storage::Reservation,
}
pub struct ChunkCache {
    pending: Option<Pending>,
    indexes: Vec<Arc<PackedIndex>>,
    chunks: Vec<Arc<Chunk>>,
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
    fn evict_one(&mut self) -> bool {
        if !self.chunks.is_empty() {
            let old = self.chunks.remove(0);
            self.stats.current -= old.cost;
            true
        } else if !self.indexes.is_empty() {
            let old = self.indexes.remove(0);
            self.stats.current -= (old.key.capacity() + old.data.capacity()) as u64;
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
    fn resize(&mut self, r: &mut mcap::storage::Reservation, n: usize) -> Outcome<()> {
        loop {
            match r.resize(n) {
                Ok(()) => return Ok(()),
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
        if let Some(i) = self.indexes.iter().position(|i| i.key == key) {
            let cached = self.indexes.remove(i);
            self.indexes.push(cached.clone());
            return Ok(cached);
        }
        if index.message_index_offsets.is_empty() {
            return Err(mcap::McapError::BadIndex.into());
        }
        let mut estimated = key.len();
        for offset in index.message_index_offsets.values() {
            let body = extended::record_body(input, *offset, records::op::MESSAGE_INDEX)?;
            estimated = estimated
                .checked_add(body.len().checked_mul(8).ok_or("Index capacity overflow")?)
                .and_then(|n| n.checked_add(1024))
                .ok_or("Index capacity overflow")?;
        }
        let mut charge = self.reserve(estimated)?;
        let mut packed = Vec::new();
        for (channel_id, offset) in &index.message_index_offsets {
            let body = extended::record_body(input, *offset, records::op::MESSAGE_INDEX)?;
            let records::Record::MessageIndex(record) =
                mcap::parse_record(records::op::MESSAGE_INDEX, body)?
            else {
                unreachable!()
            };
            if record.channel_id != *channel_id {
                return Err(mcap::McapError::BadIndex.into());
            }
            if !summary.channels.contains_key(channel_id) {
                return Err(mcap::McapError::UnknownChannel(0, *channel_id).into());
            }
            packed.try_reserve_exact(
                record
                    .records
                    .len()
                    .checked_mul(18)
                    .ok_or("Index capacity overflow")?,
            )?;
            for e in record.records {
                packed.extend_from_slice(&channel_id.to_le_bytes());
                packed.extend_from_slice(&e.log_time.to_le_bytes());
                packed.extend_from_slice(&e.offset.to_le_bytes());
            }
        }
        let cost = key
            .len()
            .checked_add(packed.capacity())
            .ok_or("Index capacity overflow")?;
        charge.resize(cost)?;
        let cached = Arc::new(PackedIndex {
            key: key.to_vec(),
            data: packed,
            _charge: charge,
        });
        if cost as u64 <= limit && limit != 0 {
            while self.stats.current.saturating_add(cost as u64) > limit {
                if !self.indexes.is_empty() {
                    let old = self.indexes.remove(0);
                    self.stats.current -= (old.key.capacity() + old.data.capacity()) as u64;
                } else if !self.chunks.is_empty() {
                    let old = self.chunks.remove(0);
                    self.stats.current -= old.cost;
                } else {
                    break;
                }
            }
            self.stats.capacity(
                self.stats.current as usize,
                self.stats.current as usize + cost,
            );
            self.indexes.push(cached.clone());
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
        if let Some(i) = self.chunks.iter().position(|c| c.key == key) {
            self.hits += 1;
            let chunk = self.chunks.remove(i);
            self.chunks.push(chunk.clone());
            return Ok(chunk);
        }
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
            self.stats.current -= c.cost;
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
            self.stats.current -= c.cost;
        }
        drop(self.reserve(self.domain.limits().block.min(4096))?);
        let mut parser =
            buffer_reader::chunk_parser(header, &data, body.len(), self.domain.clone())?;
        parser.set_memory_budget(self.domain.clone());
        let mut fed = false;
        let mut entries = Vec::<Entry>::new();
        let mut charge = self.reserve(key.len())?;
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
                        if entries.len() == entries.capacity() {
                            let capacity = entries
                                .capacity()
                                .max(16)
                                .checked_mul(2)
                                .ok_or("Index capacity overflow")?;
                            self.resize(
                                &mut charge,
                                key.len() + capacity * std::mem::size_of::<Entry>(),
                            )?;
                            entries.try_reserve_exact(capacity - entries.len())?;
                            charge.resize(
                                key.len() + entries.capacity() * std::mem::size_of::<Entry>(),
                            )?;
                        }
                        entries.push(Entry {
                            offset: position,
                            header: buffer_reader::native_header(&header),
                            data: data.slice(22..data.as_ref().len()),
                        });
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
        let cost = storage_capacity
            .saturating_add((key.len() + entries.capacity() * std::mem::size_of::<Entry>()) as u64);
        let chunk = Arc::new(Chunk {
            key: key.to_vec(),
            entries,
            cost,
            _charge: charge,
        });
        if limit != 0 && cost <= limit {
            while self.stats.current.saturating_add(cost) > limit {
                if !self.chunks.is_empty() {
                    let c = self.chunks.remove(0);
                    self.stats.current -= c.cost;
                } else {
                    let c = self.indexes.remove(0);
                    self.stats.current -= (c.key.capacity() + c.data.capacity()) as u64;
                }
            }
            self.stats.capacity(
                self.stats.current as usize,
                (self.stats.current + cost) as usize,
            );
            self.chunks.push(chunk.clone());
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
            self.stats.copied += sink.send(records::op::MESSAGE, &message.header, data)?;
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
        }
        Ok(Some(0))
    }
}
