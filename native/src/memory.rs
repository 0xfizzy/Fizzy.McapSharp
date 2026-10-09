use super::io::open_input;
use super::{io, Outcome, MessageHeader};
use std::io::{Seek, SeekFrom};
use std::ptr;
use serde_json::Value;
use super::io::Input;
use std::ops::Deref;

#[derive(Clone)]
pub struct Options {
    pub sort: Option<u64>,
    pub random: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            random: 0,
            sort: None,
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
            sort: field(v, "MaxBufferedSortBytes")?,
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
pub enum Backing {
    Owned {
        data: Vec<u8>,
    },
    Mapped {
        mapping: std::sync::Arc<io::MappedInput>,
    },
}
impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned { data, .. } => data,
            Self::Mapped { mapping, .. } => mapping,
        }
    }
}
impl AsRef<[u8]> for Backing {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
impl Backing {
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Owned { data } => data.capacity(),
            Self::Mapped { .. } => 0,
        }
    }

    pub fn empty() -> Self {
        Self::copy(&[]).unwrap()
    }
    pub fn copy(data: &[u8]) -> Outcome<Self> {
        let mut v = Vec::new();
        v.try_reserve_exact(data.len())?;
        v.extend_from_slice(data);
        Ok(Self::Owned { data: v })
    }
    pub fn open(path: &str) -> Outcome<Self> {
        match open_input(path)? {
            Input::Map { mapping, .. } => Ok(Self::Mapped { mapping }),
            _ => unreachable!(),
        }
    }
}
// A synchronous, private delivery callback. Never retained after an export returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sink {
    pub context: *mut std::ffi::c_void,
    pub accept: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        u8,
        *const MessageHeader,
        *const u8,
        usize,
        *mut usize,
    ) -> i32,
}
const _: () = assert!(std::mem::size_of::<Sink>() == 16);
impl Sink {
    pub unsafe fn send(self, opcode: u8, header: &MessageHeader, data: &[u8]) -> Outcome<u64> {
        let mut copied = 0;
        if (self.accept)(
            self.context,
            opcode,
            header,
            data.as_ptr(),
            data.len(),
            &mut copied,
        ) != crate::protocol::callback_status::ACCEPTED
        {
            return Err("Managed delivery callback failed".into());
        }
        if copied > data.len() {
            return Err("Invalid delivery copy count".into());
        }
        Ok(copied as u64)
    }
}
pub struct Delivery {
    pub shared: Option<mcap::storage::SharedBytes>,
    pub capture: bool,
    pub sink: Option<Sink>,
    pub wanted: u8,
    pub data: Vec<u8>,
    pub active: bool,
    pub opcode: u8,
    pub header: MessageHeader,
}
impl Default for Delivery {
    fn default() -> Self {
        Self {
            shared: None,
            capture: false,
            sink: None,
            wanted: 0,
            data: Vec::new(),
            active: false,
            opcode: 0,
            header: MessageHeader::default(),
        }
    }
}
impl Delivery {
    pub fn bytes(&self) -> &[u8] {
        self.shared
            .as_ref()
            .map_or(self.data.as_slice(), |data| data.as_ref())
    }
    pub fn take_shared(&mut self, start: usize) -> Outcome<mcap::storage::SharedBytes> {
        if let Some(data) = self.shared.take() {
            self.active = false;
            return Ok(data.slice(start..data.as_ref().len()));
        }
        let n = self.data.len();
        let data = std::sync::Arc::new(Backing::Owned {
            data: std::mem::take(&mut self.data),
        });
        self.active = false;
        Ok(mcap::storage::SharedBytes::external(data, start..n))
    }
    pub fn reserve(&mut self, n: usize) -> Outcome<()> {
        if n > self.data.capacity() {
            self.data.try_reserve_exact(n - self.data.len())?;
        }
        Ok(())
    }
    pub fn reserve_scratch(&mut self, n: usize) -> Outcome<()> {
        self.reserve(n)
    }
    pub fn release(&mut self) {
        self.shared = None;
        self.active = false;
        self.data.clear();
    }
    pub fn discard(&mut self) {
        self.data = Vec::new();
        self.shared = None;
        self.active = false;
    }
    pub unsafe fn send(&mut self, data: &[u8], dest: *mut u8) -> Outcome<()> {
        if let Some(sink) = self.sink {
            sink.send(self.opcode, &self.header, data)?;
        } else {
            copy(data, dest)?;
        }
        Ok(())
    }
    pub unsafe fn deliver(&mut self, data: &[u8], dest: *mut u8, capacity: usize) -> Outcome<i32> {
        if self.capture {
            return Ok(crate::protocol::status::SUCCESS);
        }
        if self.sink.is_some() {
            self.send(data, dest)?;
            self.release();
            return Ok(crate::protocol::status::SUCCESS);
        }
        if capacity < data.len() {
            if self.shared.is_none() {
                self.data.clear();
                self.reserve(data.len())?;
                self.data.extend_from_slice(data);
            }
            self.active = true;
            return Ok(crate::protocol::status::BUFFER_TOO_SMALL);
        }
        copy(data, dest)?;
        self.release();
        Ok(crate::protocol::status::SUCCESS)
    }
    pub unsafe fn retry(&mut self, dest: *mut u8, capacity: usize) -> Outcome<i32> {
        if self.capture {
            self.shared = Some(self.take_shared(0)?);
            return Ok(crate::protocol::status::SUCCESS);
        }
        if let Some(sink) = self.sink {
            sink.send(self.opcode, &self.header, self.bytes())?;
        } else {
            if capacity < self.bytes().len() {
                return Ok(crate::protocol::status::BUFFER_TOO_SMALL);
            }
            copy(self.bytes(), dest)?;
        }
        self.release();
        Ok(crate::protocol::status::SUCCESS)
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
