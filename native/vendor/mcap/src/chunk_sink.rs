use std::io::{Cursor, Seek, Write};
use crate::{io_utils::McapWrite, storage::{StorageFailure, StorageFailureKind, ResourceCategory}, McapResult};

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

impl<W: Write> McapWrite for ChunkSink<W> {
    fn write_mcap(&mut self, buf: &[u8]) -> McapResult<usize> {
        if let Some(w) = &mut self.buffer {
            let charge = self.charge.as_mut().unwrap();
            let domain = charge.domain();
            let needed = usize::try_from(w.position())
                .ok()
                .and_then(|p| p.checked_add(buf.len()))
                .ok_or_else(|| StorageFailure::at(&domain, ResourceCategory::Writer, usize::MAX,
                    "chunk-growth", StorageFailureKind::Overflow).terminal())?;
            if needed > charge.limits().block {
                let mut error = StorageFailure::at(&domain, ResourceCategory::Writer, needed,
                    "chunk-growth", StorageFailureKind::PermanentLimit).terminal();
                error.details.resource = "WriterBuffer";
                error.details.limit = charge.limits().block;
                return Err(error.into());
            }
            let v = w.get_mut();
            if needed > v.capacity() {
                let capacity = needed
                    .max(v.capacity().saturating_mul(2).max(4096))
                    .min(charge.limits().block);
                let domain = charge.domain();
                // Keep the old allocation charged until its replacement is allocated and copied.
                let (mut replacement, replacement_charge) = crate::charged::vector_fixed::<u8>(
                    &domain,
                    crate::storage::ResourceCategory::Writer,
                    capacity,
                ).map_err(StorageFailure::terminal)?;
                let growing = v.capacity() != 0;
                replacement.extend_from_slice(v);
                domain.copy_bytes(crate::storage::CopyKind::Compaction, v.len());
                *v = replacement;
                *charge = replacement_charge;
                if growing {
                    domain.reallocated();
                }
            }
            Ok(w.write(buf)?)
        } else {
            Ok(self.inner.write(buf)?)
        }
    }

    fn flush_mcap(&mut self) -> McapResult<()> {
        Ok(self.as_mut_write().flush()?)
    }
}
#[cfg(test)]
mod growth_tests {
    use super::*;
    use crate::storage::{BudgetLimits, ResourceCategory};

    #[test]
    fn buffered_growth_is_geometric_and_failure_preserves_old_storage() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {
            total: (20000) + crate::storage::BudgetRef::allocation_size(),
            block: 20000,
            retained: 0,
        }).unwrap();
        let mut sink = ChunkSink::new(
            Vec::new(),
            ChunkMode::Buffered {
                buffer: Vec::new(),
                charge: domain.reserve_class(0, ResourceCategory::Writer).unwrap(),
            },
        );
        for _ in 0..8 {
            sink.write_all_mcap(&[7; 1024]).unwrap();
        }
        assert_eq!(domain.workload_detailed_statistics().allocation_count, 2);
        assert_eq!(domain.workload_statistics().current, 8192);
        assert!(sink.write_all_mcap(&[8; 1024]).is_err());
        assert_eq!(sink.buffer.as_ref().unwrap().get_ref(), &vec![7; 8192]);
        assert_eq!(domain.workload_statistics().current, 8192);
        drop(sink);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
