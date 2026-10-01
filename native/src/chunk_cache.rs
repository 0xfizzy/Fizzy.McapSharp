//! Complete, immutable chunks with a byte-limited LRU. Storage owners survive eviction.
use super::*;
use std::cell::Cell;
use std::sync::Arc;
struct Entry {
    offset: u64,
    header: MessageHeader,
    data: mcap::storage::SharedBytes,
}
pub(super) struct Chunk {
    accessed: Cell<u128>,
    key: Vec<u8>,
    entries: Vec<Entry>,
    cost: u64,
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
    accessed: Cell<u128>,
    key: Vec<u8>,
    pub data: Vec<u8>,
}
struct Pending {
    key: Vec<u8>,
    offset: u64,
    message: lease::Message,
}
pub struct ChunkCache {
    clock: u128,
    pending: Option<Pending>,
    indexes: Vec<Arc<PackedIndex>>,
    chunks: Vec<Arc<Chunk>>,
    retained_bytes: u64,
    pub hits: u64,
    pub loads: u64,
}
impl Default for ChunkCache {
    fn default() -> Self {
        Self::new()
    }
}
impl ChunkCache {
    pub fn new() -> Self {
        Self {
            clock: 0,
            indexes: Vec::new(),
            pending: None,
            chunks: Vec::new(),
            retained_bytes: 0,
            hits: 0,
            loads: 0,
        }
    }
    pub fn clear(&mut self) {
        self.pending = None;
        self.indexes.clear();
        self.chunks.clear();
        self.retained_bytes = 0;
    }
    fn evict_one(&mut self) -> bool {
        if !self.chunks.is_empty()
            && self
                .indexes
                .first()
                .is_none_or(|index| self.chunks[0].accessed.get() <= index.accessed.get())
        {
            let old = self.chunks.remove(0);
            self.retained_bytes -= old.cost;
            true
        } else if !self.indexes.is_empty() {
            let old = self.indexes.remove(0);
            self.retained_bytes -= (old.key.capacity() + old.data.capacity()) as u64;
            true
        } else {
            false
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
        self.clock += 1;
        if let Some(i) = self.indexes.iter().position(|i| i.key == key) {
            let cached = self.indexes.remove(i);
            cached.accessed.set(self.clock);
            self.indexes.push(cached.clone());
            return Ok(cached);
        }
        if index.message_index_offsets.is_empty() {
            return Err(mcap::McapError::BadIndex.into());
        }
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
        let cached = Arc::new(PackedIndex {
            accessed: Cell::new(self.clock),
            key: key.to_vec(),
            data: packed,
        });
        if cost as u64 <= limit && limit != 0 {
            while self.retained_bytes.saturating_add(cost as u64) > limit {
                if !self.evict_one() {
                    break;
                }
            }
            self.retained_bytes += cost as u64;
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
        self.clock += 1;
        if let Some(i) = self.chunks.iter().position(|c| c.key == key) {
            self.hits += 1;
            let chunk = self.chunks.remove(i);
            chunk.accessed.set(self.clock);
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
        let mut parser = buffer_reader::chunk_parser(header, &data, body.len())?;
        let mut fed = false;
        let mut entries = Vec::<Entry>::new();
        let mut position = 0u64;
        loop {
            let event = parser.next_shared_event().transpose()?;
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
                            entries.try_reserve_exact(capacity - entries.len())?;
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
            accessed: Cell::new(self.clock),
            key: key.to_vec(),
            entries,
            cost,
        });
        if limit != 0 && cost <= limit {
            while self.retained_bytes.saturating_add(cost) > limit {
                if !self.evict_one() {
                    break;
                }
            }
            self.retained_bytes += cost;
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
            sink.send(records::op::MESSAGE, &message.header, data)?;
        } else {
            if capacity < data.len() {
                self.pending = Some(Pending {
                    key: key.to_vec(),
                    offset,
                    message,
                });
                return Ok(Some(2));
            }
            memory::copy(data, dest)?;
        }
        Ok(Some(0))
    }
}
