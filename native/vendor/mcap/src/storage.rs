//! Binding extension: stable storage for borrowed delivery and retained leases.
use std::{ops::Range, sync::Arc};
struct Allocation {
    pointer: *mut u8,
    length: usize,
}
// Only WriteBuffer mutates, and only outside every published SharedBytes range.
unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}
impl Allocation {
    fn new(bytes: Box<[u8]>) -> Self {
        let length = bytes.len();
        Self {
            pointer: Box::into_raw(bytes).cast(),
            length,
        }
    }
    unsafe fn tail(&self, range: Range<usize>) -> &mut [u8] {
        assert!(range.start <= range.end && range.end <= self.length);
        std::slice::from_raw_parts_mut(self.pointer.add(range.start), range.len())
    }
}
impl Drop for Allocation {
    fn drop(&mut self) {
        let bytes = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                self.pointer,
                self.length,
            ))
        };
        drop(bytes);
    }
}
#[derive(Clone)]
enum Backing {
    Empty,
    Owned(Arc<Allocation>),
    External(Arc<dyn AsRef<[u8]> + Send + Sync>),
}
#[derive(Clone)]
pub struct SharedBytes {
    backing: Backing,
    range: Range<usize>,
}
impl SharedBytes {
    /// Capacity of retained output storage; external mappings have no native heap capacity.
    pub fn owned_capacity(&self) -> Option<usize> {
        match &self.backing {
            Backing::Owned(b) => Some(b.length),
            Backing::External(_) | Backing::Empty => None,
        }
    }
    pub fn empty() -> Self {
        Self { backing: Backing::Empty, range: 0..0 }
    }
    pub fn external(source: Arc<dyn AsRef<[u8]> + Send + Sync>, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= source.as_ref().as_ref().len());
        Self {
            backing: Backing::External(source),
            range,
        }
    }
    pub fn slice(&self, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= self.range.len());
        Self {
            backing: self.backing.clone(),
            range: self.range.start + range.start..self.range.start + range.end,
        }
    }
}
impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        match &self.backing {
            Backing::Empty => &[],
            Backing::Owned(b) => unsafe {
                std::slice::from_raw_parts(b.pointer.add(self.range.start), self.range.len())
            },
            Backing::External(b) => &b.as_ref().as_ref()[self.range.clone()],
        }
    }
}

#[derive(Default)]
pub(crate) struct WriteBuffer {
    backing: Option<SharedBytes>,
    read_only: bool,
}
impl WriteBuffer {
    pub fn bytes(&self) -> &[u8] {
        self.backing.as_ref().map_or(&[], |b| b.as_ref())
    }
    pub fn external(&mut self, bytes: SharedBytes) {
        self.backing = Some(bytes);
        self.read_only = true;
    }
    pub fn reserve(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        if !self.read_only && self.backing.as_ref().is_some_and(|b| b.as_ref().len() >= size) {
            return Ok(());
        }
        self.relocate(size, preserve)
    }
    pub fn relocate(&mut self, size: usize, preserve: Range<usize>) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(std::io::Error::other)?;
        bytes.resize(size, 0);
        let old = &self.bytes()[preserve];
        bytes[..old.len()].copy_from_slice(old);
        self.backing = Some(SharedBytes {
            backing: Backing::Owned(Arc::new(Allocation::new(bytes.into_boxed_slice()))),
            range: 0..size,
        });
        self.read_only = false;
        Ok(())
    }
    pub fn reset(&mut self) {
        if !self.exclusive() { self.backing = None; }
    }
    pub fn exclusive(&self) -> bool {
        !self.read_only && matches!(&self.backing, Some(SharedBytes { backing: Backing::Owned(b), .. }) if Arc::strong_count(b) == 1)
    }
    // The parser may append after published ranges; overlapping writes require exclusive().
    pub unsafe fn writable(&mut self, range: Range<usize>) -> &mut [u8] {
        assert!(!self.read_only);
        match &self.backing.as_ref().unwrap().backing {
            Backing::Owned(b) => b.tail(range),
            _ => unreachable!(),
        }
    }
    pub fn share(&self, range: Range<usize>) -> SharedBytes {
        self.backing.as_ref().unwrap().slice(range)
    }
}
