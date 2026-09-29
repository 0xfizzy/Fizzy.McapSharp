use super::*;
struct Block {
    data: Vec<u8>,
    remaining: usize,
}
struct Entry {
    header: MessageHeader,
    block: usize,
    offset: usize,
    length: usize,
    ordinal: u64,
}
#[derive(Default)]
pub struct Arena {
    blocks: Vec<Block>,
    entries: Vec<Entry>,
    position: usize,
    pub stats: memory::Statistics,
}
impl Arena {
    fn reserve<T>(
        v: &mut Vec<T>,
        limit: Option<u64>,
        stats: &mut memory::Statistics,
    ) -> Outcome<()> {
        if v.len() < v.capacity() {
            return Ok(());
        }
        let size = std::mem::size_of::<T>();
        let old = v.capacity() * size;
        let available = limit
            .unwrap_or(isize::MAX as u64)
            .saturating_sub(stats.current) as usize
            / size;
        let target = (v.capacity().max(16).saturating_mul(2))
            .min(v.capacity().saturating_add(available))
            .max(v.len() + 1);
        let requested = (target - v.capacity())
            .checked_mul(size)
            .and_then(|n| (stats.current as usize).checked_add(n))
            .ok_or("Sort capacity overflow")?;
        memory::check("BufferedSort", limit, requested)?;
        v.try_reserve_exact(target - v.len())?;
        stats.capacity(old, v.capacity() * size);
        memory::check("BufferedSort", limit, stats.current as usize)
    }
    pub fn push(&mut self, header: MessageHeader, data: &[u8], limit: Option<u64>) -> Outcome<()> {
        Self::reserve(&mut self.entries, limit, &mut self.stats)?;
        let mut block = usize::MAX;
        let mut offset = 0;
        if !data.is_empty() {
            if self
                .blocks
                .last()
                .is_none_or(|b| b.data.capacity() - b.data.len() < data.len())
            {
                Self::reserve(&mut self.blocks, limit, &mut self.stats)?;
                let available = limit
                    .unwrap_or(isize::MAX as u64)
                    .saturating_sub(self.stats.current) as usize;
                let capacity = data.len().max((1024 * 1024).min(available));
                let total = (self.stats.current as usize)
                    .checked_add(capacity)
                    .ok_or("Sort capacity overflow")?;
                memory::check("BufferedSort", limit, total)?;
                let mut v = Vec::new();
                v.try_reserve_exact(capacity)?;
                self.stats.capacity(0, v.capacity());
                memory::check("BufferedSort", limit, self.stats.current as usize)?;
                self.blocks.push(Block {
                    data: v,
                    remaining: 0,
                });
            }
            block = self.blocks.len() - 1;
            let b = &mut self.blocks[block];
            offset = b.data.len();
            b.data.extend_from_slice(data);
            b.remaining += 1;
            self.stats.copied += data.len() as u64;
        }
        self.entries.push(Entry {
            header,
            block,
            offset,
            length: data.len(),
            ordinal: self.entries.len() as u64,
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
    pub unsafe fn read(
        &mut self,
        dest: *mut u8,
        capacity: usize,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<i32> {
        let Some(e) = self.entries.get(self.position) else {
            return Ok(1);
        };
        *header = e.header;
        out.value = e.length as u64;
        if capacity < e.length {
            return Ok(2);
        }
        if e.block != usize::MAX {
            let b = &mut self.blocks[e.block];
            memory::copy(&b.data[e.offset..e.offset + e.length], dest)?;
            b.remaining -= 1;
            if b.remaining == 0 {
                self.stats.capacity(b.data.capacity(), 0);
                b.data = Vec::new();
            }
        }
        self.stats.copied += e.length as u64;
        self.position += 1;
        if self.position == self.entries.len() {
            self.clear();
        }
        Ok(0)
    }
    pub fn clear(&mut self) {
        self.blocks = Vec::new();
        self.entries = Vec::new();
        self.position = 0;
        self.stats.current = 0;
    }
}
