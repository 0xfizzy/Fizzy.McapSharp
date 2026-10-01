use std::io::{self, prelude::*};

use crc32fast::Hasher;

/// Private typed output boundary: budget failures must not be boxed as I/O errors.
pub(crate) trait McapWrite {
    fn write_mcap(&mut self, bytes: &[u8]) -> crate::McapResult<usize>;
    fn flush_mcap(&mut self) -> crate::McapResult<()>;
    fn write_all_mcap(&mut self, mut bytes: &[u8]) -> crate::McapResult<()> {
        while !bytes.is_empty() {
            match self.write_mcap(bytes) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => bytes = &bytes[count..],
                Err(crate::McapError::Io(error)) if error.kind() == io::ErrorKind::Interrupted => {},
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
impl<W: McapWrite + ?Sized> McapWrite for &mut W {
    fn write_mcap(&mut self, bytes: &[u8]) -> crate::McapResult<usize> { (**self).write_mcap(bytes) }
    fn flush_mcap(&mut self) -> crate::McapResult<()> { (**self).flush_mcap() }
}
#[cfg(any(test, feature = "allocation-audit"))]
impl McapWrite for io::Sink {
    fn write_mcap(&mut self, bytes: &[u8]) -> crate::McapResult<usize> { Ok(bytes.len()) }
    fn flush_mcap(&mut self) -> crate::McapResult<()> { Ok(()) }
}
impl<W: McapWrite> McapWrite for CountingCrcWriter<W> {
    fn write_mcap(&mut self, bytes: &[u8]) -> crate::McapResult<usize> {
        self.write_with(bytes, |writer, bytes| writer.write_mcap(bytes))
    }
    fn flush_mcap(&mut self) -> crate::McapResult<()> { self.inner.flush_mcap() }
}

pub struct CountingCrcWriter<W> {
    inner: W,
    hasher: Option<Hasher>,
    count: u64,
}

impl<W> CountingCrcWriter<W> {
    pub fn new(inner: W, calculate_crc: bool) -> Self {
        Self {
            inner,
            hasher: if calculate_crc {
                Some(Hasher::new())
            } else {
                None
            },
            count: 0,
        }
    }

    pub fn with_hasher(inner: W, hasher: Option<Hasher>) -> Self {
        Self {
            inner,
            hasher,
            count: 0,
        }
    }

    pub(crate) fn write_with<E>(&mut self, buf: &[u8],
        write: impl FnOnce(&mut W, &[u8]) -> Result<usize, E>) -> Result<usize, E> {
        let count = write(&mut self.inner, buf)?;
        self.count += count as u64;
        if let Some(hasher) = &mut self.hasher { hasher.update(&buf[..count]); }
        Ok(count)
    }

    pub fn position(&self) -> u64 {
        self.count
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consumes the reader and returns the inner writer and the checksum
    pub fn finalize(self) -> (W, Option<Hasher>) {
        (self.inner, self.hasher)
    }

    pub fn current_checksum(&self) -> u32 {
        self.hasher
            .clone()
            .map(|hasher| hasher.finalize())
            .unwrap_or(0)
    }
}

impl<W: Write> Write for CountingCrcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(buf, |writer, bytes| writer.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for CountingCrcWriter<W> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        self.inner.stream_position()
    }
}
