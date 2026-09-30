use std::io::{Cursor, Seek, Write};

/// The kind of writer that should be used for writing chunks.
///
/// This is used to select what [`ChunkSink`] should be used by the MCAP writer.
#[derive(Default)]
pub(crate) enum ChunkMode {
    /// Mode specifying that chunks should be written directly to the output
    #[default]
    Direct,
    /// Mode specifying that chunks should be buffered before writing to the output
    Buffered {
        /// The reusable buffer used by the [`ChunkSink`] when writing to [`ChunkSink::Buffered`]
        buffer: Vec<u8>,
        charge: crate::storage::Reservation,
    },
}

/// The writer used for writing chunks.
///
/// If chunks are buffered they will be written to an internal buffer, which can be flushed to the
/// provided writer once the chunk is completed.
pub(crate) struct ChunkSink<W> {
    pub inner: W,
    charge: Option<crate::storage::Reservation>,
    pub buffer: Option<Cursor<Vec<u8>>>,
}

impl<W> ChunkSink<W> {
    pub fn new(writer: W, mode: ChunkMode) -> Self {
        let (buffer, charge) = match mode {
            ChunkMode::Buffered { mut buffer, charge } => {
                buffer.clear();
                (Some(Cursor::new(buffer)), Some(charge))
            }
            ChunkMode::Direct => (None, None),
        };
        Self {
            inner: writer,
            buffer,
            charge,
        }
    }
}

impl<W: Write> ChunkSink<W> {
    fn as_mut_write(&mut self) -> &mut dyn Write {
        match &mut self.buffer {
            Some(w) => w,
            None => &mut self.inner,
        }
    }

    pub fn finish(self) -> (W, std::io::Result<ChunkMode>) {
        let ChunkSink {
            mut inner,
            buffer,
            charge,
        } = self;
        let mode = match buffer {
            Some(buffer) => {
                let buffer = buffer.into_inner();
                if let Err(err) = inner.write_all(&buffer) {
                    return (inner, Err(err));
                }
                ChunkMode::Buffered {
                    buffer,
                    charge: charge.unwrap(),
                }
            }
            None => ChunkMode::Direct,
        };
        (inner, Ok(mode))
    }
}

impl<W: Seek> ChunkSink<W> {
    fn as_mut_seek(&mut self) -> &mut dyn Seek {
        match &mut self.buffer {
            Some(w) => w,
            None => &mut self.inner,
        }
    }
}

impl<W: Seek> Seek for ChunkSink<W> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.as_mut_seek().seek(pos)
    }

    fn stream_position(&mut self) -> std::io::Result<u64> {
        self.as_mut_seek().stream_position()
    }
}

impl<W: Write> Write for ChunkSink<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(w) = &mut self.buffer {
            let needed = usize::try_from(w.position())
                .ok()
                .and_then(|p| p.checked_add(buf.len()))
                .ok_or_else(|| std::io::Error::other("Chunk buffer overflow"))?;
            let charge = self.charge.as_mut().unwrap();
            if needed > charge.limits().block {
                return Err(std::io::Error::other(
                    "Chunk buffer exceeds storage block limit",
                ));
            }
            let v = w.get_mut();
            if needed > v.capacity() {
                let capacity = needed
                    .max(v.capacity().saturating_mul(2).max(4096))
                    .min(charge.limits().block);
                let domain = charge.domain();
                // Keep the old allocation charged until its replacement is allocated and copied.
                let mut replacement_charge =
                    domain.reserve_class(capacity, crate::storage::ResourceCategory::Writer)?;
                let mut replacement = Vec::new();
                replacement
                    .try_reserve_exact(capacity)
                    .map_err(std::io::Error::other)?;
                replacement_charge.resize(replacement.capacity())?;
                replacement_charge.commit(replacement.capacity());
                replacement.extend_from_slice(v);
                domain.copy_bytes(crate::storage::CopyKind::Compaction, v.len());
                *v = replacement;
                *charge = replacement_charge;
            }
            w.write(buf)
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.as_mut_write().flush()
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;
    use crate::storage::{BudgetLimits, MemoryBudget, ResourceCategory};
    use std::sync::Arc;
    #[test]
    fn buffered_growth_is_geometric_and_failure_preserves_old_storage() {
        let domain = Arc::new(MemoryBudget::new(BudgetLimits {
            total: 20000,
            block: 20000,
            retained: 0,
        }));
        let mut sink = ChunkSink::new(
            Vec::new(),
            ChunkMode::Buffered {
                buffer: Vec::new(),
                charge: domain.reserve_class(0, ResourceCategory::Writer).unwrap(),
            },
        );
        for _ in 0..8 {
            sink.write_all(&[7; 1024]).unwrap();
        }
        assert_eq!(domain.detailed_statistics().allocation_count, 2);
        assert_eq!(domain.statistics().current, 8192);
        assert!(sink.write_all(&[8; 1024]).is_err());
        assert_eq!(sink.buffer.as_ref().unwrap().get_ref(), &vec![7; 8192]);
        assert_eq!(domain.statistics().current, 8192);
        drop(sink);
        assert_eq!(domain.statistics().current, 0);
    }
}
