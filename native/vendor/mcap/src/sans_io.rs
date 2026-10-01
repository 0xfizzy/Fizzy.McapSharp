//! Read MCAP files from any source of bytes
pub mod decompressor;
pub mod indexed_reader;
pub mod linear_reader;
pub mod summary_reader;

pub use indexed_reader::{IndexedReadEvent, IndexedReader, IndexedReaderOptions};
pub use linear_reader::{LinearReadEvent, LinearReader, LinearReaderOptions};
pub use summary_reader::{SummaryReadEvent, SummaryReader, SummaryReaderOptions};

#[cfg(feature = "lz4")]
mod lz4;

#[cfg(feature = "zstd")]
mod zstd;

/// Utility function for checking u64 lengths from MCAP files.
pub(crate) fn check_len(len: u64, limit: Option<usize>) -> Option<usize> {
    let len_as_usize: usize = len.try_into().ok()?;
    match limit {
        Some(limit) if len_as_usize > limit => None,
        _ => Some(len_as_usize),
    }
}

/// Drives actual decoders for independent allocation-failure accounting tests.
/// The caller owns the source/output buffers; no test wrapper allocates storage.
#[cfg(feature = "allocation-audit")]
pub fn audit_decode_twice(name: &str, domain: crate::storage::BudgetRef, source: &[u8], output: &mut [u8])
    -> crate::McapResult<usize> {
    use decompressor::Decompressor;
    fn run(mut decoder: impl Decompressor, source: &[u8], output: &mut [u8]) -> crate::McapResult<usize> {
        let mut length = 0;
        for pass in 0..2 {
            if pass != 0 { decoder.reset()?; }
            let (mut consumed, mut wrote) = (0, 0);
            while consumed < source.len() {
                let next = decoder.decompress(&source[consumed..], &mut output[wrote..])?;
                if next.consumed == 0 && next.wrote == 0 { return Err(crate::McapError::UnexpectedEoc); }
                consumed += next.consumed;
                wrote += next.wrote;
            }
            if decoder.next_read_size() != 0 { return Err(crate::McapError::UnexpectedEoc); }
            if pass != 0 { assert_eq!(wrote, length); }
            length = wrote;
        }
        Ok(length)
    }
    match name {
        #[cfg(feature = "lz4")]
        "lz4" => run(lz4::Lz4Decoder::with_budget(domain)?, source, output),
        #[cfg(feature = "zstd")]
        "zstd" => run(zstd::ZstdDecoder::with_budget(domain)?, source, output),
        _ => unreachable!("known test codec required"),
    }
}
