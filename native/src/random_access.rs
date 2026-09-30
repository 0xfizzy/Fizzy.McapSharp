//! Optional owned caches. No parser event reference survives an ABI call.
use super::*;

#[derive(Default)]
pub struct Retry {
    key: Vec<u8>,
    data: Vec<u8>,
    op: u32,
    time: u64,
    offset: u64,
    header: MessageHeader,
    active: bool,
}
impl Retry {
    pub fn clear(&mut self, stats: &mut memory::Statistics) {
        stats.capacity(self.key.capacity() + self.data.capacity(), 0);
        *self = Self::default();
    }
    fn reset(&mut self) { self.active = false; self.key.clear(); self.data.clear(); }
    pub unsafe fn read_owned(&mut self, op: u32, key: &[u8], time: u64, offset: u64, sink: memory::Sink, stats: &mut memory::Statistics) -> Outcome<bool> {
        if !self.active { return Ok(false); }
        if self.op != op || self.key != key || self.time != time || self.offset != offset {
            self.reset(); return Ok(false);
        }
        stats.copied += sink.send(records::op::MESSAGE, &self.header, &self.data)?;
        self.reset();
        Ok(true)
    }
    pub unsafe fn read(
        &mut self,
        op: u32,
        key: &[u8],
        time: u64,
        offset: u64,
        dest: *mut u8,
        capacity: usize,
        header: *mut MessageHeader,
        out: &mut Response,
        _stats: &mut memory::Statistics,
    ) -> Outcome<Option<i32>> {
        if !self.active {
            return Ok(None);
        }
        if self.op != op || self.key != key || self.time != time || self.offset != offset {
            self.reset();
            return Ok(None);
        }
        out.value = self.data.len() as u64;
        if !header.is_null() {
            *header = self.header;
        }
        if capacity < self.data.len() {
            return Ok(Some(2));
        }
        memory::copy(&self.data, dest)?;
        // The snapshot epilogue counts successful caller-buffer delivery.
        self.reset();
        Ok(Some(0))
    }
    pub fn save_packed(&mut self, key: &[u8], time: u64, offset: u64, length: usize,
        options: memory::Options, stats: &mut memory::Statistics, encode: impl FnOnce(&mut Vec<u8>)) {
        self.reset();
        let limit = options.pending.unwrap_or(u64::MAX).min(options.retained);
        let target = self.key.capacity().max(key.len()).saturating_add(self.data.capacity().max(length));
        if target as u64 > limit { self.clear(stats); return; }
        let old_key = self.key.capacity();
        let old_data = self.data.capacity();
        let key_result = self.key.try_reserve_exact(key.len());
        stats.capacity(old_key, self.key.capacity());
        let data_result = self.data.try_reserve_exact(length);
        stats.capacity(old_data, self.data.capacity());
        if key_result.is_err() || data_result.is_err() || self.key.capacity().saturating_add(self.data.capacity()) as u64 > limit {
            self.clear(stats); return;
        }
        self.key.extend_from_slice(key);
        encode(&mut self.data);
        stats.copied += (key.len() + length) as u64;
        self.op = 5; self.time = time; self.offset = offset; self.header = MessageHeader::default(); self.active = true;
    }
    pub fn save(
        &mut self,
        op: u32,
        key: &[u8],
        time: u64,
        offset: u64,
        header: MessageHeader,
        data: Vec<u8>,
        options: memory::Options,
        stats: &mut memory::Statistics,
    ) {
        self.reset();
        let limit = options.pending.unwrap_or(u64::MAX).min(options.retained);
        let Some(total) = data.capacity().checked_add(self.key.capacity().max(key.len())) else {
            self.clear(stats); return;
        };
        if total as u64 > limit { self.clear(stats); return; }
        if key.len() > self.key.capacity() {
            let old = self.key.capacity();
            if self.key.try_reserve_exact(key.len()).is_err() { self.clear(stats); return; }
            stats.capacity(old, self.key.capacity());
        }
        if self.key.capacity().saturating_add(data.capacity()) as u64 > limit {
            self.clear(stats); return;
        }
        self.key.extend_from_slice(key);
        stats.copied += key.len() as u64;
        stats.capacity(self.data.capacity(), data.capacity());
        self.data = data;
        self.op = op; self.time = time; self.offset = offset; self.header = header;
        self.active = true;
    }
}

struct Entry {
    position: u64,
    opcode: u8,
    start: usize,
    length: usize,
}
#[derive(Default)]
pub struct ChunkCache {
    // Parser owns its buffers; backing bytes are accessed only during read().
    parser: Option<sans_io::LinearReader>,
    key: Vec<u8>,
    data: Vec<u8>,
    entries: Vec<Entry>,
    input_position: usize,
    input_end: usize,
    position: u64,
    pub stats: memory::Statistics,
    #[cfg(test)]
    pub events: u64,
}
impl ChunkCache {
    pub fn clear(&mut self) {
        self.parser = None;
        self.key = Vec::new();
        self.data = Vec::new();
        self.entries = Vec::new();
        self.position = 0;
        self.stats.current = 0;
    }
    fn reserve<T>(v: &mut Vec<T>, n: usize, limit: u64, stats: &mut memory::Statistics) -> bool {
        if n <= v.capacity() {
            return true;
        }
        let size = std::mem::size_of::<T>();
        let old = v.capacity() * size;
        let available = limit.saturating_sub(stats.current) as usize / size;
        if n.saturating_sub(v.capacity()) > available {
            return false;
        }
        let target = n.max(
            v.capacity()
                .saturating_mul(2)
                .min(v.capacity().saturating_add(available)),
        );
        if v.try_reserve_exact(target - v.len()).is_err() {
            return false;
        }
        stats.capacity(old, v.capacity() * size);
        stats.current <= limit
    }
    pub unsafe fn read(
        &mut self,
        input: &[u8],
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
        self.read_with(input, summary, index, key, offset, limit, dest, capacity, header, out, None)
    }
    pub unsafe fn read_with(
        &mut self,
        input: &[u8],
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
        if limit == 0 {
            return Ok(None);
        }
        if self.key != key || self.parser.is_none() {
            self.clear();
            if !Self::reserve(&mut self.key, key.len(), limit, &mut self.stats) {
                self.clear();
                return Ok(None);
            }
            self.key.extend_from_slice(key);
            self.stats.copied += key.len() as u64;
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
            let records::Record::Chunk { header, data } =
                mcap::parse_record(records::op::CHUNK, body)?
            else {
                unreachable!()
            };
            self.parser = Some(buffer_reader::chunk_parser(header, &data, body.len(), Default::default())?);
            self.input_position = start;
            self.input_end = end;
        }
        loop {
            if offset < self.position {
                let Ok(i) = self.entries.binary_search_by_key(&offset, |e| e.position) else {
                    self.clear();
                    return Ok(None);
                };
                let e = &self.entries[i];
                let records::Record::Message { header: h, data } =
                    mcap::parse_record(e.opcode, &self.data[e.start..e.start + e.length])?
                else {
                    return Err(mcap::McapError::BadIndex.into());
                };
                if !summary.channels.contains_key(&h.channel_id) {
                    return Err(mcap::McapError::UnknownChannel(h.sequence, h.channel_id).into());
                }
                if !header.is_null() {
                    *header = MessageHeader {
                        channel_id: h.channel_id,
                        sequence: h.sequence,
                        log_time: h.log_time,
                        publish_time: h.publish_time,
                        reserved: 0,
                    };
                }
                out.value = data.len() as u64;
                if sink.is_none() && capacity < data.len() {
                    return Ok(Some(2));
                }
                if let Some(sink) = sink {
                    let h = MessageHeader { channel_id: h.channel_id, sequence: h.sequence, log_time: h.log_time, publish_time: h.publish_time, reserved: 0 };
                    self.stats.copied += sink.send(records::op::MESSAGE, &h, &data)?;
                } else {
                    memory::copy(&data, dest)?;
                    self.stats.copied += data.len() as u64;
                }
                return Ok(Some(0));
            }
            let parser = self.parser.as_mut().ok_or("Missing cached parser")?;
            #[cfg(test)]
            {
                self.events += 1;
            }
            match parser.next_event().transpose()? {
                None => return Err(mcap::McapError::BadIndex.into()),
                Some(sans_io::LinearReadEvent::ReadRequest(n)) => {
                    let n = n.min(self.input_end - self.input_position);
                    parser
                        .insert(n)
                        .copy_from_slice(&input[self.input_position..self.input_position + n]);
                    self.stats.copied += n as u64;
                    parser.notify_read(n);
                    self.input_position += n;
                }
                Some(sans_io::LinearReadEvent::Record { opcode, data }) => {
                    let n = self
                        .data
                        .len()
                        .checked_add(data.len())
                        .ok_or("Cache length overflow")?;
                    let count = self.entries.len() + 1;
                    if !Self::reserve(&mut self.entries, count, limit, &mut self.stats)
                        || !Self::reserve(&mut self.data, n, limit, &mut self.stats)
                    {
                        self.clear();
                        return Ok(None);
                    }
                    self.entries.push(Entry {
                        position: self.position,
                        opcode,
                        start: self.data.len(),
                        length: data.len(),
                    });
                    self.data.extend_from_slice(data);
                    self.stats.copied += data.len() as u64;
                    self.position = self
                        .position
                        .checked_add(9)
                        .and_then(|n| n.checked_add(data.len() as u64))
                        .ok_or(mcap::McapError::BadIndex)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_seeks_match_official_and_hits_do_not_advance() {
        for compression in [
            None,
            Some(mcap::Compression::Lz4),
            Some(mcap::Compression::Zstd),
        ] {
            let mut writer = mcap::WriteOptions::new()
                .compression(compression)
                .chunk_size(None)
                .create(std::io::Cursor::new(Vec::new()))
                .unwrap();
            let channel = writer.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
            for sequence in 0..32 {
                writer
                    .write_to_known_channel(
                        &records::MessageHeader {
                            channel_id: channel,
                            sequence,
                            log_time: sequence as u64,
                            publish_time: 0,
                        },
                        &[42; 128],
                    )
                    .unwrap();
            }
            writer.finish().unwrap();
            let data = writer.into_inner().into_inner();
            let summary = mcap::Summary::read(&data).unwrap().unwrap();
            let index = &summary.chunk_indexes[0];
            let key = buffer_reader::encode(records::Record::ChunkIndex(index.clone()))
                .unwrap()
                .1;
            let entries = summary.read_message_indexes(&data, index).unwrap();
            let entries = entries.values().next().unwrap();
            let mut cache = ChunkCache::default();
            let mut header = MessageHeader::default();
            let mut response = Response::default();
            let mut output = [0; 128];
            for e in entries.iter().rev().chain(entries.iter()) {
                let expected = summary.seek_message(&data, index, e).unwrap();
                unsafe {
                    assert_eq!(
                        cache
                            .read(
                                &data,
                                &summary,
                                index,
                                &key,
                                e.offset,
                                65536,
                                output.as_mut_ptr(),
                                output.len(),
                                &mut header,
                                &mut response
                            )
                            .unwrap(),
                        Some(0)
                    );
                    assert_eq!(header.sequence, expected.sequence);
                    assert_eq!(output.as_slice(), expected.data.as_ref());
                    let events = cache.events;
                    assert_eq!(
                        cache
                            .read(
                                &data,
                                &summary,
                                index,
                                &key,
                                e.offset,
                                65536,
                                output.as_mut_ptr(),
                                output.len(),
                                &mut header,
                                &mut response
                            )
                            .unwrap(),
                        Some(0)
                    );
                    assert_eq!(events, cache.events);
                }
            }
            for offset in [0, entries[0].offset + 1, u64::MAX] {
                let expected = summary.seek_message(
                    &data,
                    index,
                    &records::MessageIndexEntry {
                        log_time: 0,
                        offset,
                    },
                );
                let actual = unsafe {
                    cache.read(
                        &data,
                        &summary,
                        index,
                        &key,
                        offset,
                        65536,
                        output.as_mut_ptr(),
                        output.len(),
                        &mut header,
                        &mut response,
                    )
                };
                // Interior offsets fall back to the official helper to preserve deferred errors.
                if !matches!(actual, Ok(None)) {
                    assert_eq!(expected.is_err(), actual.is_err());
                }
            }
            for corrupt_at in [
                index.chunk_start_offset as usize + 9 + 24,
                (index.chunk_start_offset + index.chunk_length - 1) as usize,
            ] {
                let mut damaged = data.clone();
                damaged[corrupt_at] ^= 1;
                for e in [entries.first().unwrap(), entries.last().unwrap()] {
                    let expected = summary.seek_message(&damaged, index, e);
                    let mut damaged_cache = ChunkCache::default();
                    let actual = unsafe {
                        damaged_cache.read(
                            &damaged,
                            &summary,
                            index,
                            &key,
                            e.offset,
                            65536,
                            output.as_mut_ptr(),
                            output.len(),
                            &mut header,
                            &mut response,
                        )
                    };
                    match (expected, actual) {
                        (Ok(m), Ok(Some(0))) => {
                            assert_eq!(header.sequence, m.sequence);
                            assert_eq!(output.as_slice(), m.data.as_ref());
                        }
                        (Err(a), Err(b)) => {
                            assert_eq!(errors::encode(&a), errors::encode(b.as_ref()))
                        }
                        (a, b) => panic!("Corrupted tail mismatch: {a:?} / {b:?}"),
                    }
                }
            }
            cache.clear();
            assert_eq!(cache.stats.current, 0);
            unsafe {
                assert_eq!(
                    cache
                        .read(
                            &data,
                            &summary,
                            index,
                            &key,
                            entries[0].offset,
                            1,
                            output.as_mut_ptr(),
                            output.len(),
                            &mut header,
                            &mut response
                        )
                        .unwrap(),
                    None
                );
            }
            assert_eq!(cache.stats.current, 0);
        }
    }
    #[test]
    fn retry_identity_and_capacity_boundaries() {
        for limit in [0, 6, 7] {
            let mut retry = Retry::default();
            let mut stats = memory::Statistics::default();
            let mut options = memory::Options::default();
            options.pending = Some(limit);
            retry.save(
                2,
                &[1, 2, 3],
                4,
                5,
                MessageHeader::default(),
                vec![9; 4],
                options,
                &mut stats,
            );
            assert_eq!(retry.active, limit == 7);
            let mut out = Response::default();
            let mut output = [0; 4];
            unsafe {
                if retry.active {
                    let allocations = stats.allocations;
                    assert_eq!(
                        retry
                            .read(
                                2,
                                &[1, 2, 3],
                                4,
                                5,
                                ptr::null_mut(),
                                0,
                                ptr::null_mut(),
                                &mut out,
                                &mut stats
                            )
                            .unwrap(),
                        Some(2)
                    );
                    assert_eq!(
                        retry
                            .read(
                                2,
                                &[1, 2, 3],
                                4,
                                5,
                                output.as_mut_ptr(),
                                4,
                                ptr::null_mut(),
                                &mut out,
                                &mut stats
                            )
                            .unwrap(),
                        Some(0)
                    );
                    assert_eq!(allocations, stats.allocations);
                    assert_eq!(output, [9; 4]);
                }
            }
            assert_eq!(stats.current, if limit == 7 { 7 } else { 0 });
            retry.clear(&mut stats);
            assert_eq!(stats.current, 0);
        }
    }
}
