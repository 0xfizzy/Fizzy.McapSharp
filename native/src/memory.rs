use super::*;
use std::ops::Deref;

#[derive(Clone, Copy)]
pub struct Options {
    pub owned: Option<u64>,
    pub pending: Option<u64>,
    pub sort: Option<u64>,
    pub retained: u64,
    pub random: u64,
    pub scratch: Option<u64>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            random: 0,
            scratch: None,
            owned: None,
            pending: None,
            sort: None,
            retained: 8 * 1024 * 1024,
        }
    }
}
impl Options {
    pub fn parse(v: &Value) -> Outcome<Self> {
        fn field(v: &Value, key: &str) -> Outcome<Option<u64>> {
            if v[key].is_null() {
                Ok(None)
            } else {
                Ok(Some(v[key].as_u64().ok_or("Invalid memory limit")?))
            }
        }
        Ok(Self {
            random: field(v, "MaxRandomAccessCacheBytes")?.unwrap_or(0),
            scratch: field(v, "MaxScratchBufferBytes")?,
            owned: field(v, "MaxOwnedInputBytes")?,
            pending: field(v, "MaxPendingBufferBytes")?,
            sort: field(v, "MaxBufferedSortBytes")?,
            retained: field(v, "MaxRetainedBufferBytes")?.unwrap_or(8 * 1024 * 1024),
        })
    }
}
#[derive(Debug)]
pub struct Limit {
    pub resource: &'static str,
    pub limit: u64,
    pub requested: u64,
}
impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} memory limit {} exceeded by requested capacity {}",
            self.resource, self.limit, self.requested
        )
    }
}
impl std::error::Error for Limit {}
pub fn check(resource: &'static str, limit: Option<u64>, requested: usize) -> Outcome<()> {
    if let Some(limit) = limit {
        if requested as u64 > limit {
            return Err(Box::new(Limit {
                resource,
                limit,
                requested: requested as u64,
            }));
        }
    }
    if requested > isize::MAX as usize {
        return Err("Capacity overflow".into());
    }
    Ok(())
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Statistics {
    pub current: u64,
    pub peak: u64,
    pub allocations: u64,
    pub copied: u64,
    pub mapped: u64,
}
impl Statistics {
    pub fn capacity(&mut self, old: usize, new: usize) {
        self.current = self.current - old as u64 + new as u64;
        self.peak = self.peak.max(self.current);
        if new > old {
            self.allocations += 1;
        }
    }
    pub fn with_input(mut self, input: &Backing) -> Self {
        let n = input.capacity() as u64;
        self.current += n;
        self.peak += n;
        self.allocations += u64::from(n != 0);
        self.copied += if n != 0 { input.len() as u64 } else { 0 };
        self.mapped = input.mapped();
        self
    }
}
pub enum Backing {
    Owned(Vec<u8>),
    Mapped { mapping: Mmap, _file: File },
}
impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped { mapping, .. } => mapping,
        }
    }
}
impl Backing {
    pub fn capacity(&self) -> usize {
        match self {
            Self::Owned(v) => v.capacity(),
            _ => 0,
        }
    }
    pub fn mapped(&self) -> u64 {
        match self {
            Self::Mapped { mapping, .. } => mapping.len() as u64,
            _ => 0,
        }
    }
    pub fn copy(data: &[u8], options: Options) -> Outcome<Self> {
        check("OwnedInput", options.owned, data.len())?;
        let mut v = Vec::new();
        v.try_reserve_exact(data.len())?;
        check("OwnedInput", options.owned, v.capacity())?;
        v.extend_from_slice(data);
        Ok(Self::Owned(v))
    }
    pub fn open(path: &str) -> Outcome<Self> {
        match open_input(path)? {
            Input::Map { mapping, _file, .. } => Ok(Self::Mapped { mapping, _file }),
            _ => unreachable!(),
        }
    }
}
// A synchronous, private delivery callback. Never retained after an export returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sink {
    pub context: *mut std::ffi::c_void,
    pub accept: unsafe extern "C" fn(*mut std::ffi::c_void, u8, *const MessageHeader, *const u8, usize, *mut usize) -> i32,
}
const _: () = assert!(std::mem::size_of::<Sink>() == 16);
impl Sink {
    pub unsafe fn send(self, opcode: u8, header: &MessageHeader, data: &[u8]) -> Outcome<u64> {
        let mut copied = 0;
        if (self.accept)(self.context, opcode, header, data.as_ptr(), data.len(), &mut copied) != 0 {
            return Err("Managed delivery callback failed".into());
        }
        if copied > data.len() { return Err("Invalid delivery copy count".into()); }
        Ok(copied as u64)
    }
}
pub struct Delivery {
    pub sink: Option<Sink>,
    pub wanted: u8,
    pub data: Vec<u8>,
    pub active: bool,
    pub opcode: u8,
    pub header: MessageHeader,
    pub options: Options,
    pub stats: Statistics,
}
impl Default for Delivery {
    fn default() -> Self {
        Self {
            sink: None,
            wanted: 0,
            data: Vec::new(),
            active: false,
            opcode: 0,
            header: MessageHeader::default(),
            options: Options::default(),
            stats: Statistics::default(),
        }
    }
}
impl Delivery {
    pub fn reserve(&mut self, n: usize) -> Outcome<()> {
        self.reserve_resource(n, "PendingBuffer", self.options.pending)
    }
    pub fn reserve_scratch(&mut self, n: usize) -> Outcome<()> {
        self.reserve_resource(n, "ScratchBuffer", self.options.scratch)
    }
    fn reserve_resource(&mut self, n: usize, resource: &'static str, limit: Option<u64>) -> Outcome<()> {
        if n > self.data.capacity() {
            check(resource, limit, n)?;
            let old = self.data.capacity();
            self.data.try_reserve_exact(n - self.data.len())?;
            self.stats.capacity(old, self.data.capacity());
            check(resource, limit, self.data.capacity())?;
        }
        Ok(())
    }
    pub fn release(&mut self) {
        self.active = false;
        self.data.clear();
        if self.data.capacity() as u64 > self.options.retained {
            self.discard();
        }
    }
    pub fn discard(&mut self) {
        self.stats.capacity(self.data.capacity(), 0);
        self.data = Vec::new();
        self.active = false;
    }
    pub unsafe fn send(&mut self, data: &[u8], dest: *mut u8) -> Outcome<()> {
        if let Some(sink) = self.sink {
            self.stats.copied += sink.send(self.opcode, &self.header, data)?;
        } else {
            copy(data, dest)?;
            self.stats.copied += data.len() as u64;
        }
        Ok(())
    }
    pub unsafe fn deliver(&mut self, data: &[u8], dest: *mut u8, capacity: usize) -> Outcome<i32> {
        if self.sink.is_some() {
            self.send(data, dest)?;
            self.release();
            return Ok(0);
        }
        if capacity < data.len() {
            self.data.clear();
            self.reserve(data.len())?;
            self.data.extend_from_slice(data);
            self.stats.copied += data.len() as u64;
            self.active = true;
            return Ok(2);
        }
        copy(data, dest)?;
        self.stats.copied += data.len() as u64;
        self.release();
        Ok(0)
    }
    pub unsafe fn retry(&mut self, dest: *mut u8, capacity: usize) -> Outcome<i32> {
        if let Some(sink) = self.sink {
            self.stats.copied += sink.send(self.opcode, &self.header, &self.data)?;
        } else {
            if capacity < self.data.len() { return Ok(2); }
            copy(&self.data, dest)?;
            self.stats.copied += self.data.len() as u64;
        }
        self.release();
        Ok(0)
    }
}
pub unsafe fn copy(data: &[u8], dest: *mut u8) -> Outcome<()> {
    if !data.is_empty() {
        if dest.is_null() {
            return Err("Null destination".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
    }
    Ok(())
}

#[derive(Default)]
pub struct Measure {
    position: u64,
    pub length: u64,
}
impl std::io::Write for Measure {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.position = self
            .position
            .checked_add(data.len() as u64)
            .ok_or_else(|| std::io::Error::other("Encoding size overflow"))?;
        self.length = self.length.max(self.position);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Seek for Measure {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let n = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => self.length as i128 + n as i128,
        };
        self.position =
            u64::try_from(n).map_err(|_| std::io::Error::other("Encoding offset overflow"))?;
        Ok(self.position)
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_memory_statistics(
    kind: u32,
    p: *const std::ffi::c_void,
    stats: *mut Statistics,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if p.is_null() || stats.is_null() {
            return Err("Null memory statistics argument".into());
        }
        *stats = match kind {
            0 => {
                let r = &*(p as *const Reader);
                let mut s = r.delivery.stats;
                s.current += r.scratch.stats.current;
                s.allocations += r.scratch.stats.allocations;
                s.copied += r.scratch.stats.copied;
                s.current += r.arena.stats.current;
                s.peak = r.memory_peak.max(s.current);
                s.allocations += r.arena.stats.allocations;
                s.copied += r.arena.stats.copied;
                if let Input::Map { mapping, .. } = &r.input {
                    s.mapped = mapping.len() as u64;
                }
                s
            }
            1 => {
                let r = &*(p as *const buffer_reader::BufferReader);
                r.delivery.stats.with_input(&r.input)
            }
            2 => {
                let r = &*(p as *const extended::Snapshot);
                {
                    let mut s = Statistics::default();
                    s.current += r.stats.current + r.cache.stats.current;
                    s.allocations += r.cache.stats.allocations;
                    s.copied += r.cache.stats.copied;
                    s.peak = r.memory_peak.max(s.current);
                    s.allocations += r.stats.allocations;
                    s.copied += r.stats.copied;
                    s.with_input(&r.data)
                }
            }
            3 => (&*(p as *const extended::EngineHandle)).delivery.stats,
            _ => return Err("Unknown memory statistics source".into()),
        };
        Ok(0)
    })
}

const _: () = assert!(std::mem::size_of::<Statistics>() == 40);
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_capacity_and_reusable_delivery() {
        assert!(check("PendingBuffer", None, usize::MAX).is_err());
        let mut d = Delivery::default();
        d.options.pending = Some(16);
        unsafe {
            assert_eq!(d.deliver(&[1; 16], ptr::null_mut(), 0).unwrap(), 2);
            let counters = d.stats;
            assert_eq!(d.retry(ptr::null_mut(), 0).unwrap(), 2);
            assert_eq!(d.stats.allocations, counters.allocations);
            let mut output = [0; 16];
            assert_eq!(d.retry(output.as_mut_ptr(), 16).unwrap(), 0);
            assert_eq!(output, [1; 16]);
            assert_eq!(d.deliver(&[2; 16], ptr::null_mut(), 0).unwrap(), 2);
            assert_eq!(d.stats.allocations, counters.allocations);
            d.release();
            assert!(d.deliver(&[3; 17], ptr::null_mut(), 0).is_err());
            d.discard();
            assert_eq!(d.stats.current, 0);
        }
    }
}
