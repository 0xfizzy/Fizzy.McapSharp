use super::{memory, Outcome, Response, MessageHeader};
#[cfg(test)]
use std::ptr;
use mcap::records;
#[cfg(test)]
use mcap::sans_io;
const COMPACT_TARGET: usize = 256 * 1024;
struct Entry {
    data: Option<mcap::storage::SharedBytes>,
    header: MessageHeader,
    ordinal: usize,
}
/// Local fallback-sort allowance: logical payload bytes plus descriptor capacity.
/// Shared storage may retain larger upstream chunks; this is not a heap limit.
#[derive(Default)]
pub struct Arena {
    entries: Vec<Entry>,
    position: usize,
    payload_bytes: usize,
    group_owner: Option<mcap::storage::SharedBytes>,
    group_start: usize,
    group_bytes: usize,
    #[cfg(test)]
    pub copied_bytes: usize,
    #[cfg(test)]
    pub peak_compaction_overlap: usize,
    #[cfg(test)]
    pub shared_baseline: bool,
}
impl Arena {
    pub fn push_shared(
        &mut self,
        header: MessageHeader,
        data: mcap::storage::SharedBytes,
        options: &memory::Options,
    ) -> Outcome<()> {
        let size = std::mem::size_of::<Entry>();
        let payload_bytes = self
            .payload_bytes
            .checked_add(data.as_ref().len())
            .ok_or("Sort overflow")?;
        let capacity = if self.entries.len() == self.entries.capacity() {
            self.entries
                .capacity()
                .max(16)
                .checked_mul(2)
                .ok_or("Sort overflow")?
        } else {
            self.entries.capacity()
        };
        let requested = capacity
            .checked_mul(size)
            .and_then(|n| n.checked_add(payload_bytes))
            .ok_or("Sort overflow")?;
        memory::check("BufferedSort", options.sort, requested)?;
        if capacity > self.entries.capacity() {
            self.entries
                .try_reserve_exact(capacity - self.entries.len())?;
            let actual = self
                .entries
                .capacity()
                .checked_mul(size)
                .and_then(|n| n.checked_add(payload_bytes))
                .ok_or("Sort overflow")?;
            memory::check("BufferedSort", options.sort, actual)?;
        }
        if self
            .group_owner
            .as_ref()
            .is_some_and(|owner| !owner.shares_backing(&data))
        {
            self.finish_group()?;
        }
        self.payload_bytes = payload_bytes;
        if !data.as_ref().is_empty() && data.owned_capacity().is_some() {
            if self.group_owner.is_none() {
                self.group_start = self.entries.len();
                self.group_owner = Some(data.clone());
            }
            self.group_bytes += data.as_ref().len();
        }
        let data = if data.as_ref().is_empty() {
            mcap::storage::SharedBytes::empty()
        } else {
            data
        };
        self.entries.push(Entry {
            data: Some(data),
            header,
            ordinal: self.entries.len(),
        });
        Ok(())
    }
    fn finish_group(&mut self) -> Outcome<()> {
        let Some(owner) = self.group_owner.take() else {
            return Ok(());
        };
        let capacity = owner.owned_capacity().unwrap();
        let selected = std::mem::take(&mut self.group_bytes);
        #[cfg(test)]
        if self.shared_baseline {
            return Ok(());
        }
        if selected > capacity / 4 || capacity.saturating_sub(selected) < COMPACT_TARGET {
            return Ok(());
        }
        // The old owner remains alive until all group references have been replaced.
        // Allocate exact message-aligned segments; never repack a previously compacted group.
        let mut start = self.group_start;
        #[cfg(test)]
        let mut compact_capacity = 0;
        while start < self.entries.len() {
            let mut end = start;
            let mut length = 0;
            while end < self.entries.len() {
                let n = self.entries[end].data.as_ref().unwrap().as_ref().len();
                if end > start && length + n > COMPACT_TARGET {
                    break;
                }
                length += n;
                end += 1;
                if length >= COMPACT_TARGET {
                    break;
                }
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(length)?;
            for entry in &self.entries[start..end] {
                let payload = entry.data.as_ref().unwrap().as_ref();
                bytes.extend_from_slice(payload);
                #[cfg(test)]
                {
                    self.copied_bytes += payload.len();
                }
            }
            #[cfg(test)]
            {
                compact_capacity += bytes.capacity();
            }
            let backing = std::sync::Arc::new(memory::Backing::Owned { data: bytes });
            let mut offset = 0;
            for entry in &mut self.entries[start..end] {
                let n = entry.data.as_ref().unwrap().as_ref().len();
                entry.data = Some(if n == 0 {
                    mcap::storage::SharedBytes::empty()
                } else {
                    mcap::storage::SharedBytes::external(backing.clone(), offset..offset + n)
                });
                offset += n;
            }
            start = end;
        }
        #[cfg(test)]
        {
            self.peak_compaction_overlap = self
                .peak_compaction_overlap
                .max(capacity + compact_capacity);
        }
        Ok(())
    }
    pub fn sort(&mut self, reverse: bool) -> Outcome<()> {
        self.finish_group()?;
        self.entries
            .sort_unstable_by_key(|e| (e.header.log_time, e.ordinal));
        if reverse {
            self.entries.reverse();
        }
        Ok(())
    }
    fn pending(&self, header: &mut MessageHeader, out: &mut Response) -> Option<&[u8]> {
        let e = self.entries.get(self.position)?;
        *header = e.header;
        let data = e.data.as_ref().unwrap().as_ref();
        out.value = data.len() as u64;
        Some(data)
    }
    pub fn read_shared(
        &mut self,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Option<mcap::storage::SharedBytes> {
        self.pending(header, out)?;
        let data = self.entries[self.position].data.take().unwrap();
        self.position += 1;
        if self.position == self.entries.len() {
            self.clear();
        }
        Some(data)
    }
    pub unsafe fn read(
        &mut self,
        dest: *mut u8,
        capacity: usize,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<i32> {
        let Some(data) = self.pending(header, out) else {
            return Ok(crate::protocol::status::END);
        };
        if capacity < data.len() {
            return Ok(crate::protocol::status::BUFFER_TOO_SMALL);
        }
        memory::copy(data, dest)?;
        self.read_shared(header, out);
        Ok(crate::protocol::status::SUCCESS)
    }
    pub unsafe fn read_owned(
        &mut self,
        sink: memory::Sink,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<i32> {
        let Some(data) = self.pending(header, out) else {
            return Ok(crate::protocol::status::END);
        };
        sink.send(records::op::MESSAGE, header, data)?;
        self.read_shared(header, out);
        Ok(crate::protocol::status::SUCCESS)
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn owned(size: usize) -> mcap::storage::SharedBytes {
        let mut parser = sans_io::LinearReader::new_with_options(
            sans_io::LinearReaderOptions::default()
                .with_skip_start_magic(true)
                .with_skip_end_magic(true),
        );
        let dest = parser.try_insert(size + 9).unwrap();
        dest[0] = 0x80;
        dest[1..9].copy_from_slice(&(size as u64).to_le_bytes());
        dest[9..].fill(42);
        parser.notify_read(size + 9);
        let Some(Ok(sans_io::linear_reader::SharedReadEvent::Record { data, .. })) =
            parser.next_shared_event()
        else {
            panic!("expected record")
        };
        data
    }
    #[test]
    fn compaction_thresholds_and_backing_identity() {
        for selected in [87381, 131072] {
            // Owned capacity includes the synthetic 9-byte record prefix.
            let threshold = (selected * 4).max(selected + COMPACT_TARGET);
            for capacity in [threshold - 1, threshold, threshold + 1] {
                let source = owned(capacity - 9);
                assert_eq!(source.owned_capacity(), Some(capacity));
                assert!(source.shares_backing(&source.slice(1..2)));
                assert!(!source.shares_backing(&owned(capacity - 9)));
                let mut arena = Arena::default();
                arena
                    .push_shared(
                        MessageHeader::default(),
                        source.slice(0..selected),
                        &memory::Options::default(),
                    )
                    .unwrap();
                arena.sort(false).unwrap();
                let compact = capacity >= threshold;
                assert_eq!(arena.copied_bytes, if compact { selected } else { 0 });
                let result = arena
                    .read_shared(&mut MessageHeader::default(), &mut Response::default())
                    .unwrap();
                assert_eq!(result.shares_backing(&source), !compact);
                assert_eq!(result.as_ref(), &source.as_ref()[..selected]);
                drop(source);
                assert!(result.as_ref().iter().all(|b| *b == 42));
            }
        }
    }
    #[test]
    fn groups_finalize_before_eof_and_segments_preserve_messages() {
        let source = owned(4 * 1024 * 1024);
        let mut arena = Arena::default();
        let sizes = [0, 128 * 1024, 128 * 1024, 300 * 1024, 1];
        for (i, n) in sizes.into_iter().enumerate() {
            let header = MessageHeader {
                sequence: i as u32,
                log_time: 1,
                ..Default::default()
            };
            arena
                .push_shared(header, source.slice(0..n), &memory::Options::default())
                .unwrap();
        }
        // A different owner closes the previous group before the whole scan completes.
        let external = std::sync::Arc::new(vec![7; 10]);
        let other = mcap::storage::SharedBytes::external(external, 0..10);
        arena
            .push_shared(
                MessageHeader {
                    sequence: 5,
                    log_time: 1,
                    ..Default::default()
                },
                other.clone(),
                &memory::Options::default(),
            )
            .unwrap();
        assert_eq!(arena.copied_bytes, sizes.iter().sum::<usize>());
        assert!(arena.entries[1]
            .data
            .as_ref()
            .unwrap()
            .shares_backing(arena.entries[2].data.as_ref().unwrap()));
        assert!(!arena.entries[2]
            .data
            .as_ref()
            .unwrap()
            .shares_backing(arena.entries[3].data.as_ref().unwrap()));
        assert!(!arena.entries[3]
            .data
            .as_ref()
            .unwrap()
            .shares_backing(arena.entries[4].data.as_ref().unwrap()));
        assert_eq!(arena.entries[0].data.as_ref().unwrap().as_ref().len(), 0);
        arena.sort(true).unwrap();
        let copied = arena.copied_bytes;
        drop(source);
        let mut header = MessageHeader::default();
        let mut out = Response::default();
        let mut retained = Vec::new();
        for i in (0..6).rev() {
            if i != 0 {
                assert_eq!(
                    unsafe {
                        arena
                            .read(ptr::null_mut(), 0, &mut header, &mut out)
                            .unwrap()
                    },
                    2
                );
                assert_eq!(header.sequence, i);
            }
            retained.push(arena.read_shared(&mut header, &mut out).unwrap());
            assert_eq!(header.sequence, i);
        }
        drop(arena);
        assert_eq!(retained[0].as_ref(), &[7; 10]);
        assert_eq!(retained[2].as_ref().len(), 300 * 1024);
        assert!(retained[2].as_ref().iter().all(|b| *b == 42));
        assert_eq!(copied, sizes.iter().sum::<usize>());
    }
    #[test]
    fn empty_results_release_owner_and_limit_semantics_stay_logical() {
        let source = owned(1024 * 1024);
        let mut arena = Arena::default();
        arena
            .push_shared(
                MessageHeader::default(),
                source.slice(0..0),
                &memory::Options::default(),
            )
            .unwrap();
        assert!(arena.group_owner.is_none());
        assert!(!arena.entries[0]
            .data
            .as_ref()
            .unwrap()
            .shares_backing(&source));
        let logical = 32 * std::mem::size_of::<Entry>() + 10;
        for limit in [logical - 1, logical] {
            let mut arena = Arena::default();
            assert_eq!(
                arena
                    .push_shared(
                        MessageHeader::default(),
                        source.slice(0..10),
                        &memory::Options {
                            sort: Some(limit as u64),
                            random: 0
                        }
                    )
                    .is_ok(),
                limit == logical
            );
        }
    }
    #[test]
    fn shared_sort_preserves_storage_and_rejects_local_limit() {
        let source = std::sync::Arc::new(vec![1u8, 2, 3]);
        let data = mcap::storage::SharedBytes::external(source.clone(), 0..3);
        let mut arena = Arena::default();
        let h = MessageHeader::default();
        assert!(arena
            .push_shared(
                h,
                data.clone(),
                &memory::Options {
                    sort: Some(0),
                    random: 0
                }
            )
            .is_err());
        arena
            .push_shared(h, data, &memory::Options::default())
            .unwrap();
        arena.sort(false).unwrap();
        let mut out = Response::default();
        let mut header = MessageHeader::default();
        assert_eq!(
            unsafe {
                arena
                    .read(ptr::null_mut(), 0, &mut header, &mut out)
                    .unwrap()
            },
            2
        );
        let retained = arena.read_shared(&mut header, &mut out).unwrap();
        assert_eq!(retained.as_ref().as_ptr(), source.as_ptr());
        assert!(arena.read_shared(&mut header, &mut out).is_none());
    }
}
