use super::*;
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
        self.payload_bytes = payload_bytes;
        self.entries.push(Entry {
            data: Some(data),
            header,
            ordinal: self.entries.len(),
        });
        Ok(())
    }
    pub fn sort(&mut self, reverse: bool) {
        self.entries
            .sort_unstable_by_key(|e| (e.header.log_time, e.ordinal));
        if reverse {
            self.entries.reverse();
        }
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
            return Ok(1);
        };
        if capacity < data.len() {
            return Ok(2);
        }
        memory::copy(data, dest)?;
        self.read_shared(header, out);
        Ok(0)
    }
    pub unsafe fn read_owned(
        &mut self,
        sink: memory::Sink,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<i32> {
        let Some(data) = self.pending(header, out) else {
            return Ok(1);
        };
        sink.send(records::op::MESSAGE, header, data)?;
        self.read_shared(header, out);
        Ok(0)
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
        arena.sort(false);
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
