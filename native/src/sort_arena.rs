use super::*;
struct Block {
    data: std::sync::Arc<Vec<u8>>,
    remaining: usize,
}
struct Entry {
    shared: Option<mcap::storage::SharedBytes>,
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
    small_block: Option<usize>,
    charge:Option<mcap::storage::Reservation>,
    shared_bytes:usize,
    pub stats: memory::Statistics,
}
impl Arena {
    #[cfg(test)]
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
    #[cfg(test)]
    pub fn push(&mut self, header: MessageHeader, data: &[u8], limit: Option<u64>) -> Outcome<()> {
        Self::reserve(&mut self.entries, limit, &mut self.stats)?;
        let mut block = usize::MAX;
        let mut offset = 0;
        if !data.is_empty() {
            let large = data.len() > 1024 * 1024;
            if large || self.small_block
                .is_none_or(|i| self.blocks[i].data.capacity() - self.blocks[i].data.len() < data.len())
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
                self.blocks.push(Block { data: std::sync::Arc::new(v), remaining: 0 });
                if !large { self.small_block = Some(self.blocks.len() - 1); }
            }
            block = if large { self.blocks.len() - 1 } else { self.small_block.unwrap() };
            let b = &mut self.blocks[block];
            offset = b.data.len();
            std::sync::Arc::get_mut(&mut b.data).unwrap().extend_from_slice(data);
            b.remaining += 1;
            self.stats.copied += data.len() as u64;
        }
        self.entries.push(Entry {
            shared:None,
            header,
            block,
            offset,
            length: data.len(),
            ordinal: self.entries.len() as u64,
        });
        Ok(())
    }
    pub fn push_shared(&mut self,header:MessageHeader,data:mcap::storage::SharedBytes,options:&memory::Options)->Outcome<()> {
        let size=std::mem::size_of::<Entry>();
        let length=data.as_ref().len();
        let capacity=if self.entries.len()==self.entries.capacity() {self.entries.capacity().max(16).checked_mul(2).ok_or("Sort overflow")?} else {self.entries.capacity()};
        let logical=self.shared_bytes.checked_add(length).and_then(|n|n.checked_add(capacity*size)).ok_or("Sort overflow")?;
        memory::check("BufferedSort",options.sort,logical)?;
        if self.charge.is_none(){self.charge=Some(options.domain.reserve(0)?);}
        self.charge.as_mut().unwrap().resize(capacity*size)?;
        if capacity>self.entries.capacity() {
            let old=self.entries.capacity()*size;
            self.entries.try_reserve_exact(capacity-self.entries.len())?;
            self.charge.as_mut().unwrap().resize(self.entries.capacity()*size)?;
            self.stats.capacity(old,self.entries.capacity()*size);
        }
        self.shared_bytes+=length;
        self.entries.push(Entry {shared:Some(data),header,block:usize::MAX,offset:0,length,ordinal:self.entries.len() as u64});
        Ok(())
    }
    pub fn sort(&mut self, reverse: bool) {
        self.entries
            .sort_unstable_by_key(|e| (e.header.log_time, e.ordinal));
        if reverse {
            self.entries.reverse();
        }
    }
    pub fn read_shared(&mut self, header: &mut MessageHeader, out: &mut Response) -> Option<mcap::storage::SharedBytes> {
        let e=self.entries.get_mut(self.position)?;
        *header=e.header; out.value=e.length as u64;
        let data=if let Some(data)=e.shared.take() {data} else if e.block==usize::MAX { mcap::storage::SharedBytes::empty() } else {
            let b=&mut self.blocks[e.block];
            let data=mcap::storage::SharedBytes::external(b.data.clone(),e.offset..e.offset+e.length);
            b.remaining-=1;
            if b.remaining==0 { self.stats.capacity(b.data.capacity(),0); b.data=Default::default(); }
            data
        };
        self.position+=1;
        if self.position==self.entries.len() { self.clear(); }
        Some(data)
    }
    pub unsafe fn read(
        &mut self,
        dest: *mut u8,
        capacity: usize,
        header: &mut MessageHeader,
        out: &mut Response,
    ) -> Outcome<i32> {
        let Some(e) = self.entries.get_mut(self.position) else {
            return Ok(1);
        };
        *header = e.header;
        out.value = e.length as u64;
        if capacity < e.length {
            return Ok(2);
        }
        if let Some(data)=e.shared.take() { memory::copy(data.as_ref(),dest)?; }
        else if e.block != usize::MAX {
            let b = &mut self.blocks[e.block];
            memory::copy(&b.data[e.offset..e.offset + e.length], dest)?;
            b.remaining -= 1;
            if b.remaining == 0 {
                self.stats.capacity(b.data.capacity(), 0);
                b.data = std::sync::Arc::new(Vec::new());
            }
        }
        self.stats.copied += e.length as u64;
        self.position += 1;
        if self.position == self.entries.len() {
            self.clear();
        }
        Ok(0)
    }
    pub unsafe fn read_owned(&mut self, sink: memory::Sink, header: &mut MessageHeader, out: &mut Response) -> Outcome<i32> {
        let Some(e) = self.entries.get_mut(self.position) else { return Ok(1); };
        *header = e.header;
        out.value = e.length as u64;
        let shared=e.shared.take();
        let data = if let Some(data)=&shared { data.as_ref() } else if e.block == usize::MAX { &[][..] } else { &self.blocks[e.block].data[e.offset..e.offset + e.length] };
        self.stats.copied += sink.send(records::op::MESSAGE, header, data)?;
        if e.block != usize::MAX {
            let b = &mut self.blocks[e.block];
            b.remaining -= 1;
            if b.remaining == 0 { self.stats.capacity(b.data.capacity(), 0); b.data = std::sync::Arc::new(Vec::new()); }
        }
        self.position += 1;
        if self.position == self.entries.len() { self.clear(); }
        Ok(0)
    }
    pub fn clear(&mut self) {
        self.blocks = Vec::new();
        self.entries = Vec::new();
        self.position = 0;
        self.small_block = None;
        self.charge=None;self.shared_bytes=0;
        self.stats.current = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_messages_do_not_take_over_the_small_block() {
        let mut arena = Arena::default();
        let h = |time| MessageHeader { log_time: time, ..Default::default() };
        arena.push(h(2), &[1; 128], None).unwrap();
        let small = arena.small_block.unwrap();
        arena.push(h(0), &vec![2; 2 * 1024 * 1024], None).unwrap();
        arena.push(h(3), &[3; 128], None).unwrap();
        assert_eq!(arena.small_block, Some(small));
        assert_eq!(arena.blocks.len(), 2);
        arena.sort(false);
        let before = arena.stats.current;
        let mut output = vec![0; 2 * 1024 * 1024];
        unsafe { arena.read(output.as_mut_ptr(), output.len(), &mut MessageHeader::default(), &mut Response::default()).unwrap(); }
        assert_eq!(before - arena.stats.current, 2 * 1024 * 1024);
        arena.clear(); assert_eq!(arena.stats.current, 0);
    }
}
