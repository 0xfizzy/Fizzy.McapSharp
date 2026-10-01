//! Contains a [sans-io](https://sans-io.readthedocs.io/) MCAP reader struct, [`LinearReader`].
//! This can be used to read MCAP data from any source of bytes.

use super::decompressor::Decompressor;
use crate::{
    records::{op, ChunkHeader},
    sans_io::check_len,
    McapError, McapResult, MAGIC,
};
use binrw::BinReaderExt;

#[cfg(feature = "lz4")]
use super::lz4;

#[cfg(feature = "zstd")]
use super::zstd;

#[derive(Clone)]
enum CurrentlyReading {
    StartMagic,
    FileRecord,
    ChunkHeader {
        len: u64,
    },
    Footer {
        len: u64,
        hasher: Option<crc32fast::Hasher>,
    },
    DataEnd {
        len: u64,
        calculated: Option<u32>,
    },
    ValidatingChunkCrc,
    ChunkRecord,
    PaddingAfterChunk,
    EndMagic,
    AfterEndMagic,
}
use CurrentlyReading::*;

// Only the two supported codecs can occupy these inline cache slots. Keeping
// adapters inline removes the uncharged Box, hash table and per-chunk string key.
enum Decoder {
    #[cfg(feature = "lz4")]
    Lz4(lz4::Lz4Decoder),
    #[cfg(feature = "zstd")]
    Zstd(zstd::ZstdDecoder),
}
impl std::ops::Deref for Decoder {
    type Target = dyn Decompressor;
    fn deref(&self) -> &Self::Target {
        #[cfg(not(any(feature = "lz4", feature = "zstd")))]
        {
            match *self {}
        }
        #[cfg(any(feature = "lz4", feature = "zstd"))]
        match self {
            #[cfg(feature = "lz4")]
            Self::Lz4(decoder) => decoder,
            #[cfg(feature = "zstd")]
            Self::Zstd(decoder) => decoder,
        }
    }
}
impl std::ops::DerefMut for Decoder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        #[cfg(not(any(feature = "lz4", feature = "zstd")))]
        {
            match *self {}
        }
        #[cfg(any(feature = "lz4", feature = "zstd"))]
        match self {
            #[cfg(feature = "lz4")]
            Self::Lz4(decoder) => decoder,
            #[cfg(feature = "zstd")]
            Self::Zstd(decoder) => decoder,
        }
    }
}

struct ChunkState {
    // The decompressor to use for loading records for this chunk. None if not compressed.
    decompressor: Option<Decoder>,
    // For uncompressed chunks, records are sliced directly out of `file_data`.  Therefore we can't
    // use `decompressed_content.hasher` to calculate a CRC, so we maintain a separate hasher for
    // this purpose.
    uncompressed_data_hasher: Option<crc32fast::Hasher>,
    // The number of compressed bytes left in the chunk that have not been read out of `file_data`.
    compressed_remaining: u64,
    // The number of uncompressed bytes left in the chunk that have not been decompressed yet.
    uncompressed_remaining: u64,
    // The total uncompressed length of the chunk records field.
    uncompressed_len: u64,
    // The number of bytes in the chunk record after the `records` field ends.
    padding_after_compressed_data: usize,
    // The CRC value that was read at the start of the chunk record.
    crc: u32,
}

// MCAP records start with an opcode (1 byte) and a 64-bit length (8 bytes).
const OPCODE_LEN_SIZE: usize = 1 + 8;

mod rw_buf {
    use crate::storage::{SharedBytes, StorageFailure, StorageFailureKind, WriteBuffer};

    #[derive(Default)]
    pub struct RwBuf {
        data: WriteBuffer,
        start: usize,
        end: usize,
        writable_end: usize,
        hasher: Option<crc32fast::Hasher>,
    }
    impl RwBuf {
        pub fn with_budget(hash: bool, budget: crate::storage::BudgetRef, category: crate::storage::ResourceCategory) -> Self {
            Self {
                data: WriteBuffer::with_budget(budget, category),
                start: 0, end: 0, writable_end: 0,
                hasher: hash.then(crc32fast::Hasher::new),
            }
        }
        pub fn category(&mut self, category: crate::storage::ResourceCategory) {
            self.data.category = category;
        }
        pub fn domain(&self) -> crate::storage::BudgetRef {
            self.data.budget.clone()
        }
        pub fn budget(&mut self, budget: crate::storage::BudgetRef) {
            self.data.budget = budget;
        }
        pub fn shared_input(&mut self, data: SharedBytes) {
            assert_eq!(self.len(), 0);
            self.end = data.as_ref().len();
            self.start = 0;
            self.writable_end = self.end;
            self.data.external(data);
        }
        pub fn share(&self, p: *const u8, n: usize) -> Option<SharedBytes> {
            let start = (p as usize).checked_sub(self.data.bytes().as_ptr() as usize)?;
            let end = start.checked_add(n)?;
            (end <= self.end).then(|| self.data.share(start..end))
        }
        pub fn mark_read(&mut self, n: usize) {
            assert!(n <= self.len());
            if let Some(h) = &mut self.hasher {
                h.update(&self.data.bytes()[self.start..self.start + n]);
            }
            self.start += n;
        }
        pub fn mark_written(&mut self, n: usize) {
            assert!(n <= self.unwritten().len());
            self.end += n;
        }
        pub fn len(&self) -> usize {
            self.end - self.start
        }
        pub fn unread(&self) -> &[u8] {
            &self.data.bytes()[self.start..self.end]
        }
        pub fn unwritten(&self) -> &[u8] {
            &self.data.bytes()[self.end..self.writable_end]
        }
        pub fn unwritten_mut(&mut self) -> &mut [u8] {
            unsafe { self.data.writable(self.end..self.writable_end) }
        }
        fn overflow(&self) -> StorageFailure {
            StorageFailure::at(&self.data.budget, self.data.category, usize::MAX,
                "parser buffer size", StorageFailureKind::Overflow)
        }
        pub fn reserve_exact(&mut self, n: usize) -> Result<(), StorageFailure> {
            let writable_end = self.end.checked_add(n).ok_or_else(|| self.overflow())?;
            self.data.reserve_fixed(writable_end, 0..self.end)?;
            self.writable_end = writable_end;
            Ok(())
        }
        pub fn reserve_chunk(&mut self, n: usize) -> Result<(), StorageFailure> {
            self.data.reserve_fixed(n, 0..self.end)
        }
        pub fn tail_with_size(&mut self, n: usize) -> Result<&mut [u8], StorageFailure> {
            // No unread bytes require a parser pin. Release it before trying to
            // allocate, so an external lease can actually unblock a retry.
            if self.len() == 0 {
                self.clear();
            } else if self.start > 4096 && self.start > self.len() {
                if self.data.exclusive() {
                    let end = self.end;
                    unsafe {
                        self.data.writable(0..end).copy_within(self.start..end, 0);
                    }
                    self.data
                        .budget
                        .copy_bytes(crate::storage::CopyKind::Compaction, self.len());
                } else {
                    let size = self
                        .len()
                        .checked_add(n)
                        .ok_or_else(|| self.overflow())?;
                    self.data.relocate_fixed(size, self.start..self.end)?;
                }
                self.end -= self.start;
                self.start = 0;
            }
            self.reserve_exact(n)?;
            Ok(self.unwritten_mut())
        }
        pub fn clear(&mut self) {
            self.data.reset();
            self.start = 0;
            self.end = 0;
            self.writable_end = 0;
        }
        pub fn consume(&mut self, n: usize) -> &[u8] {
            let start = self.start;
            self.mark_read(n);
            &self.data.bytes()[start..start + n]
        }
        pub fn consume_without_hashing(&mut self, n: usize) -> &[u8] {
            assert!(n <= self.len());
            let start = self.start;
            self.start += n;
            &self.data.bytes()[start..start + n]
        }
        pub fn hasher_mut(&mut self) -> &mut Option<crc32fast::Hasher> {
            &mut self.hasher
        }
        #[cfg(test)]
        pub fn buffer(&self) -> &[u8] {
            self.data.bytes()
        }
    }
}

use rw_buf::RwBuf;

/// Options for initializing [`LinearReader`].
#[derive(Debug, Default, Clone)]
pub struct LinearReaderOptions {
    /// If true, the reader will not expect the MCAP magic at the start of the stream.
    pub skip_start_magic: bool,
    /// If true, the reader will not expect the MCAP magic after the footer record.
    pub skip_end_magic: bool,
    /// If `skip_end_magic` is false and this is true, the reader will check that there are no
    /// bytes after the end magic.
    pub check_finishes_after_end_magic: bool,
    /// If true, the reader will yield entire chunk records. Otherwise, the reader will decompress
    /// and read into chunks, yielding the records inside.
    pub emit_chunks: bool,
    /// Enables chunk CRC validation. Ignored if `prevalidate_chunk_crcs: true`.
    pub validate_chunk_crcs: bool,
    /// Enables chunk CRC validation before yielding any messages from the chunk. Implies
    /// `validate_chunk_crcs: true`.
    pub prevalidate_chunk_crcs: bool,
    /// Enables data section CRC validation.
    pub validate_data_section_crc: bool,
    /// Enables summary section CRC validation.
    pub validate_summary_section_crc: bool,
    /// If Some(limit), the reader will return an error on any non-chunk record with length > `limit`.
    /// If used in conjunction with `prevalidate_chunk_crcs`, the reader will return an error on any
    /// chunk record where the compressed OR decompressed length are > `limit`.
    pub record_length_limit: Option<usize>,
}

impl LinearReaderOptions {
    pub fn with_skip_start_magic(mut self, skip_start_magic: bool) -> Self {
        self.skip_start_magic = skip_start_magic;
        self
    }
    pub fn with_skip_end_magic(mut self, skip_end_magic: bool) -> Self {
        self.skip_end_magic = skip_end_magic;
        self
    }
    pub fn with_check_finishes_after_end_magic(
        mut self,
        check_finishes_after_end_magic: bool,
    ) -> Self {
        self.check_finishes_after_end_magic = check_finishes_after_end_magic;
        self
    }
    pub fn with_emit_chunks(mut self, emit_chunks: bool) -> Self {
        self.emit_chunks = emit_chunks;
        self
    }
    pub fn with_validate_chunk_crcs(mut self, validate_chunk_crcs: bool) -> Self {
        self.validate_chunk_crcs = validate_chunk_crcs;
        self
    }
    pub fn with_prevalidate_chunk_crcs(mut self, prevalidate_chunk_crcs: bool) -> Self {
        self.prevalidate_chunk_crcs = prevalidate_chunk_crcs;
        self
    }
    pub fn with_validate_data_section_crc(mut self, validate_data_section_crc: bool) -> Self {
        self.validate_data_section_crc = validate_data_section_crc;
        self
    }
    pub fn with_validate_summary_section_crc(mut self, validate_summary_section_crc: bool) -> Self {
        self.validate_summary_section_crc = validate_summary_section_crc;
        self
    }

    pub fn with_record_length_limit(mut self, record_length_limit: usize) -> Self {
        self.record_length_limit = Some(record_length_limit);
        self
    }
}

/// Reads an MCAP file from start to end, yielding raw records by opcode and data buffer.
///
/// This struct does not perform any I/O on its own, instead it yields slices to the caller and
/// allows them to use their own I/O primitives.
/// ```no_run
/// use std::fs;
///
/// use tokio::fs::File as AsyncFile;
/// use tokio::io::AsyncReadExt;
/// use std::io::Read;
///
/// use mcap::sans_io::linear_reader::LinearReadEvent;
/// use mcap::McapResult;
///
/// // Asynchronously...
/// async fn read_async() -> McapResult<()> {
///     let mut file = AsyncFile::open("in.mcap").await.expect("couldn't open file");
///     let mut reader = mcap::sans_io::linear_reader::LinearReader::new();
///     while let Some(event) = reader.next_event() {
///         match event? {
///             LinearReadEvent::ReadRequest(need) => {
///                 let written = file.read(reader.insert(need)).await?;
///                 reader.notify_read(written);
///             },
///             LinearReadEvent::Record{ opcode, data } => {
///                 let raw_record = mcap::parse_record(opcode, data)?;
///                 // do something with the record...
///             }
///         }
///     }
///     Ok(())
/// }
///
/// // Or synchronously.
/// fn read_sync() -> McapResult<()> {
///     let mut file = fs::File::open("in.mcap")?;
///     let mut reader = mcap::sans_io::linear_reader::LinearReader::new();
///     while let Some(event) = reader.next_event() {
///         match event? {
///             LinearReadEvent::ReadRequest(need) => {
///                 let written = file.read(reader.insert(need))?;
///                 reader.notify_read(written);
///             },
///             LinearReadEvent::Record { opcode, data } => {
///                 let raw_record = mcap::parse_record(opcode, data)?;
///                 // do something with the record...
///             }
///         }
///     }
///     Ok(())
/// }
/// ```
pub struct LinearReader {
    // The core state of the LinearReader state machine. Describes the part of the MCAP
    // file currently being read.
    currently_reading: CurrentlyReading,
    // Auxiliary state specific to reading records out of chunks. This is stored outside of
    // CurrentlyReading to avoid needing to clone() it on every iteration.
    chunk_state: Option<ChunkState>,
    // MCAP data loaded from the file
    file_data: RwBuf,
    // data decompressed from compressed chunks
    decompressed_content: RwBuf,
    // decompressor that can be re-used between chunks.
    decompressors: [Option<Decoder>; 2],
    // Stores the number of bytes written into this reader since the last `next_event()` call.
    options: LinearReaderOptions,
    at_eof: bool,
    pending_capacity: Option<usize>,
    budget_locked: bool,
}

impl Default for LinearReader {
    fn default() -> Self {
        Self::new_with_options(Default::default())
    }
}

impl LinearReader {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn new_with_options(options: LinearReaderOptions) -> Self {
        Self::new_with_options_and_budget(options, Default::default())
    }

    /// Construct directly in the selected resource domain without temporary domains.
    pub fn new_with_options_and_budget(
        options: LinearReaderOptions,
        budget: crate::storage::BudgetRef,
    ) -> Self {
        LinearReader {
            currently_reading: if options.skip_start_magic {
                FileRecord
            } else {
                StartMagic
            },
            file_data: RwBuf::with_budget(options.validate_data_section_crc, budget.clone(), crate::storage::ResourceCategory::Input),
            decompressed_content: RwBuf::with_budget(
                options.validate_chunk_crcs && !options.prevalidate_chunk_crcs,
                budget, crate::storage::ResourceCategory::Decompressed,
            ),
            options,
            chunk_state: None,
            decompressors: Default::default(),
            at_eof: false,
            pending_capacity: None,
            budget_locked: false,
        }
    }

    /// Constructs a linear reader that will iterate through all records in a chunk.
    pub(crate) fn for_chunk(header: ChunkHeader) -> McapResult<Self> {
        Self::for_chunk_with_budget(header, crate::storage::BudgetRef::try_default()?)
    }
    pub(crate) fn for_chunk_with_budget(
        header: ChunkHeader,
        budget: crate::storage::BudgetRef,
    ) -> McapResult<Self> {
        let mut result = Self::new_with_options_and_budget(
            LinearReaderOptions::default()
                .with_skip_end_magic(true)
                .with_skip_start_magic(true)
                .with_validate_chunk_crcs(true),
            budget.clone(),
        );
        result.budget_locked = true;
        result.currently_reading = ChunkRecord;
        result.chunk_state = Some(ChunkState {
            decompressor: get_decompressor(&mut [None, None], &header.compression, budget)?,
            crc: header.uncompressed_crc,
            uncompressed_data_hasher: Some(crc32fast::Hasher::new()),
            uncompressed_len: header.uncompressed_size,
            compressed_remaining: header.compressed_size,
            uncompressed_remaining: header.uncompressed_size,
            padding_after_compressed_data: 0,
        });
        Ok(result)
    }

    /// Get a mutable slice to write new MCAP data into. Call [`Self::notify_read`] afterwards with
    /// the number of bytes successfully written.
    pub fn insert(&mut self, to_write: usize) -> &mut [u8] {
        self.try_insert(to_write)
            .expect("parser storage allocation failed")
    }

    /// Fallible input allocation with the configured storage budget.
    pub fn try_insert(&mut self, size: usize) -> McapResult<&mut [u8]> {
        let data = self.file_data.tail_with_size(size)?;
        self.budget_locked = true;
        Ok(data)
    }
    /// Select the resource domain before feeding input.
    pub fn set_memory_budget(
        &mut self,
        budget: crate::storage::BudgetRef,
    ) -> McapResult<()> {
        if crate::storage::BudgetRef::ptr_eq(&self.file_data.domain(), &budget) {
            return Ok(());
        }
        if self.budget_locked {
            return Err(std::io::Error::other(
                "Parser memory domain is immutable after allocation or advancement",
            )
            .into());
        }
        self.file_data.budget(budget.clone());
        self.decompressed_content.budget(budget);
        self.decompressed_content
            .category(crate::storage::ResourceCategory::Decompressed);
        Ok(())
    }
    /// Transfer immutable input without copying. Requires no unread input.
    pub fn supply_shared(&mut self, data: crate::storage::SharedBytes) {
        self.budget_locked = true;
        if data.as_ref().is_empty() {
            self.at_eof = true;
        }
        self.file_data.shared_input(data);
    }
    /// Advance with a stable owner for a record; it may outlive this parser.
    pub fn next_shared_event(&mut self) -> Option<McapResult<SharedReadEvent>> {
        let (opcode, pointer, length) = match self.next_event()? {
            Err(e) => return Some(Err(e)),
            Ok(LinearReadEvent::ReadRequest(n)) => {
                return Some(Ok(SharedReadEvent::ReadRequest(n)))
            }
            Ok(LinearReadEvent::Record { opcode, data }) => (opcode, data.as_ptr(), data.len()),
        };
        let data = self
            .file_data
            .share(pointer, length)
            .or_else(|| self.decompressed_content.share(pointer, length))
            .expect("event must belong to a parser buffer");
        Some(Ok(SharedReadEvent::Record { opcode, data }))
    }

    /// Notify the number of bytes read into the linear reader
    /// since the last [`Self::next_event`] call. Providing 0 indicates EOF to the reader.
    ///
    /// Panics if `read` is greater than the last `to_write` provided to [`Self::insert`].
    pub fn notify_read(&mut self, written: usize) {
        self.budget_locked = true;
        if written == 0 {
            self.at_eof = true;
        }
        if self.file_data.unwritten().len() < written {
            panic!("notify_read called with n > last inserted length");
        }
        self.file_data.mark_written(written);
    }

    /// Yields the next event the caller should take to progress through the file.
    pub fn next_event(&mut self) -> Option<McapResult<LinearReadEvent<'_>>> {
        self.budget_locked = true;
        if self.at_eof {
            // At EOF. If the reader is not expecting end magic or has already seen it, and it isn't
            // in the middle of a record or chunk, this is OK.
            if (self.options.skip_end_magic || matches!(self.currently_reading, AfterEndMagic))
                && self.file_data.len() == 0
                && self.decompressed_content.len() == 0
                && self.chunk_state.is_none()
            {
                return None;
            }
            return Some(Err(McapError::UnexpectedEof));
        }
        /// Macros for loading data into the reader. These return early with Read(n) if
        /// more data is needed.
        ///
        /// load ensures that $n bytes are available unread in self.file_data.
        macro_rules! load {
            ($n:expr) => {{
                if self.file_data.len() < $n {
                    return Some(Ok(LinearReadEvent::ReadRequest($n - self.file_data.len())));
                }
                &self.file_data.unread()[..$n]
            }};
        }

        // consume ensures that $n bytes are available in file_data, and marks them as read before
        // returning a slice containing those bytes.
        macro_rules! consume {
            ($n:expr) => {{
                if self.file_data.len() < $n {
                    return Some(Ok(LinearReadEvent::ReadRequest($n - self.file_data.len())));
                }
                self.file_data.consume($n)
            }};
        }

        // decompress ensures that $n bytes are available in the uncompressed_content buffer.
        macro_rules! decompress {
            ($n: expr, $chunk_state: expr, $decompressor:expr) => {{
                match decompress_inner(
                    $decompressor,
                    $n,
                    &mut self.file_data,
                    &mut self.decompressed_content,
                    &mut $chunk_state.compressed_remaining,
                    &mut $chunk_state.uncompressed_remaining,
                ) {
                    Ok(None) => &self.decompressed_content.unread()[..$n],
                    Ok(Some(n)) => return Some(Ok(LinearReadEvent::ReadRequest(n))),
                    Err(err) => return Some(Err(err)),
                }
            }};
        }

        // returns early on error. This is similar to the ? operator, but it returns an
        // Option<Result> instead of a Result.
        macro_rules! check {
            ($t:expr) => {
                match $t {
                    Ok(t) => t,
                    Err(err) => return Some(Err(err.into())),
                }
            };
        }

        loop {
            if let Some(n) = self.pending_capacity {
                check!(self.decompressed_content.reserve_chunk(n));
                self.pending_capacity = None;
            }
            match self.currently_reading.clone() {
                StartMagic => {
                    if !self.options.skip_start_magic {
                        let data = consume!(MAGIC.len());
                        if *data != *MAGIC {
                            return Some(Err(McapError::BadMagic));
                        }
                    }
                    self.currently_reading = CurrentlyReading::FileRecord;
                }
                FileRecord => {
                    let opcode_length_buf = load!(OPCODE_LEN_SIZE);
                    let opcode = opcode_length_buf[0];
                    let len = u64::from_le_bytes(opcode_length_buf[1..].try_into().unwrap());
                    // Some record types are handled specially.
                    if opcode == op::CHUNK && !self.options.emit_chunks {
                        self.file_data.mark_read(OPCODE_LEN_SIZE);
                        self.currently_reading = CurrentlyReading::ChunkHeader { len };
                        continue;
                    } else if opcode == op::DATA_END {
                        // The data end CRC needs to be checked against the CRC of the entire file
                        // up to the end of the previous record. We `take()` the data section hasher
                        // here before calling `mark_read()`, which would otherwise include too
                        // much data in the CRC.
                        let calculated = self
                            .file_data
                            .hasher_mut()
                            .take()
                            .map(|hasher| hasher.finalize());
                        self.file_data.mark_read(OPCODE_LEN_SIZE);
                        self.currently_reading = DataEnd { len, calculated };
                        continue;
                    } else if opcode == op::FOOTER {
                        // The summary section CRC needs to be checked against the CRC of the entire
                        // summary section _including_ the first bytes of the footer record.
                        self.file_data.mark_read(OPCODE_LEN_SIZE);
                        self.currently_reading = Footer {
                            len,
                            hasher: self.file_data.hasher_mut().take(),
                        };
                        continue;
                    }
                    // For all other records, load the entire record into memory and yield to the
                    // caller.
                    let len = check!(check_len(len, self.options.record_length_limit)
                        .ok_or(McapError::RecordTooLarge { opcode, len }));
                    let data = &consume!(OPCODE_LEN_SIZE + len)[OPCODE_LEN_SIZE..];
                    return Some(Ok(LinearReadEvent::Record { data, opcode }));
                }
                CurrentlyReading::DataEnd { len, calculated } => {
                    let len = check!(check_len(len, self.options.record_length_limit).ok_or(
                        McapError::RecordTooLarge {
                            opcode: op::DATA_END,
                            len
                        }
                    ));
                    let rec: crate::records::DataEnd =
                        check!(std::io::Cursor::new(load!(len)).read_le());
                    let saved = rec.data_section_crc;
                    if let Some(calculated) = calculated {
                        if saved != 0 && calculated != saved {
                            return Some(Err(McapError::BadDataCrc { saved, calculated }));
                        }
                    }
                    if self.options.validate_summary_section_crc {
                        *self.file_data.hasher_mut() = Some(crc32fast::Hasher::new());
                    }
                    let data = self.file_data.consume_without_hashing(len);
                    self.currently_reading = FileRecord;
                    return Some(Ok(LinearReadEvent::Record {
                        data,
                        opcode: op::DATA_END,
                    }));
                }
                CurrentlyReading::Footer { len, hasher } => {
                    let len = check!(check_len(len, self.options.record_length_limit).ok_or(
                        McapError::RecordTooLarge {
                            opcode: op::FOOTER,
                            len
                        }
                    ));
                    let data = consume!(len);
                    let footer: crate::records::Footer =
                        check!(std::io::Cursor::new(data).read_le());
                    if let Some(mut hasher) = hasher {
                        // Check the CRC of all bytes up to the CRC bytes in the footer
                        // record.
                        hasher.update(&data[..16]);
                        let calculated = hasher.finalize();
                        let saved = footer.summary_crc;
                        if saved != 0 && saved != calculated {
                            return Some(Err(McapError::BadSummaryCrc { saved, calculated }));
                        }
                    }
                    self.currently_reading = EndMagic;
                    return Some(Ok(LinearReadEvent::Record {
                        data,
                        opcode: op::FOOTER,
                    }));
                }
                CurrentlyReading::ChunkHeader { len } => {
                    // Load the chunk header from the file. The chunk header is of variable length,
                    // depending on the length of the compression string field, so we load
                    // enough bytes to read that length, then load more if necessary.
                    const MIN_CHUNK_HEADER_SIZE: usize = 8  // start time
                                                       + 8  // end time
                                                       + 8  // uncompressed size
                                                       + 4  // uncompressed CRC
                                                       + 4  // compression string length
                                                       + 8; // compressed size

                    let min_header_buf = load!(MIN_CHUNK_HEADER_SIZE);
                    let compression_len =
                        u32::from_le_bytes(min_header_buf[28..32].try_into().unwrap());
                    let header_len = MIN_CHUNK_HEADER_SIZE + compression_len as usize;
                    let domain = self.file_data.domain();
                    let header_buf = consume!(header_len);
                    let header = check!(crate::records::BorrowedChunkHeader::read(header_buf));
                    // Re-use or construct a compressor
                    let decompressor = check!(get_decompressor(
                        &mut self.decompressors,
                        header.compression,
                        domain
                    ));

                    let chunk_data_len = check!(len
                        .checked_sub(header_len as u64)
                        .ok_or(McapError::UnexpectedEoc));

                    let padding_after_compressed_data = check!(check_len(
                        check!(chunk_data_len
                            .checked_sub(header.compressed_size)
                            .ok_or(McapError::UnexpectedEoc)),
                        self.options.record_length_limit
                    )
                    .ok_or(McapError::ChunkTooLarge(len)));

                    let state = ChunkState {
                        decompressor,
                        uncompressed_data_hasher: if self.options.validate_chunk_crcs
                            && !self.options.prevalidate_chunk_crcs
                            && header.uncompressed_crc != 0
                            && header.compression.is_empty()
                        {
                            Some(crc32fast::Hasher::new())
                        } else {
                            None
                        },
                        compressed_remaining: header.compressed_size,
                        uncompressed_len: header.uncompressed_size,
                        uncompressed_remaining: header.uncompressed_size,
                        padding_after_compressed_data,
                        crc: header.uncompressed_crc,
                    };
                    self.decompressed_content.clear();
                    self.pending_capacity = if state.decompressor.is_some() {
                        Some(check!(usize::try_from(state.uncompressed_len).map_err(
                            |_| McapError::ChunkTooLarge(state.uncompressed_len)
                        )))
                    } else {
                        None
                    };
                    *self.decompressed_content.hasher_mut() = if self.options.validate_chunk_crcs
                        && !self.options.prevalidate_chunk_crcs
                        && state.crc != 0
                        && !header.compression.is_empty()
                    {
                        Some(crc32fast::Hasher::new())
                    } else {
                        None
                    };
                    if self.options.prevalidate_chunk_crcs && state.crc != 0 {
                        self.currently_reading = ValidatingChunkCrc;
                    } else {
                        self.currently_reading = ChunkRecord;
                    }
                    self.chunk_state = Some(state);
                }
                ValidatingChunkCrc => {
                    // decompress all chunk records into memory and check their CRC before yielding
                    // records.
                    let state = self
                        .chunk_state
                        .as_mut()
                        .expect("chunk state should be set");
                    match &mut state.decompressor {
                        None => {
                            let to_load = check!(check_len(
                                state.compressed_remaining,
                                self.options.record_length_limit
                            )
                            .ok_or(McapError::ChunkTooLarge(state.compressed_remaining)));
                            let records = load!(to_load);
                            let calculated = crc32fast::hash(records);
                            let saved = state.crc;
                            if calculated != saved {
                                return Some(Err(McapError::BadChunkCrc { saved, calculated }));
                            }
                            self.currently_reading = ChunkRecord;
                        }
                        Some(decompressor) => {
                            // decompress all available compressed data until there are no compressed
                            // bytes remaining. If not all of the compressed bytes are available in
                            // file_data, decompress! will return a Read event for more data.
                            let uncompressed_len = check!(check_len(
                                state.uncompressed_len,
                                self.options.record_length_limit
                            )
                            .ok_or(McapError::ChunkTooLarge(state.uncompressed_len)));
                            if self.decompressed_content.len() >= uncompressed_len {
                                let calculated =
                                    crc32fast::hash(self.decompressed_content.unread());
                                let saved = state.crc;
                                if calculated != saved {
                                    return Some(Err(McapError::BadChunkCrc { saved, calculated }));
                                }
                                self.currently_reading = ChunkRecord;
                                continue;
                            }
                            let _ = decompress!(uncompressed_len, state, decompressor);
                        }
                    }
                }
                ChunkRecord => {
                    let state = self
                        .chunk_state
                        .as_mut()
                        .expect("chunk state should be set");
                    match &mut state.decompressor {
                        None => {
                            if state.compressed_remaining == 0 {
                                self.currently_reading = PaddingAfterChunk;
                                continue;
                            }
                            let opcode_len_buf = load!(OPCODE_LEN_SIZE);
                            let opcode = opcode_len_buf[0];
                            let len = u64::from_le_bytes(opcode_len_buf[1..].try_into().unwrap());
                            let len = check!(check_len(len, self.options.record_length_limit)
                                .ok_or(McapError::RecordTooLarge { opcode, len }));
                            let opcode_len_data = consume!(OPCODE_LEN_SIZE + len);
                            let data = &opcode_len_data[OPCODE_LEN_SIZE..];
                            if let Some(hasher) = state.uncompressed_data_hasher.as_mut() {
                                hasher.update(opcode_len_data);
                            }
                            state.compressed_remaining -= (OPCODE_LEN_SIZE + len) as u64;
                            return Some(Ok(LinearReadEvent::Record { data, opcode }));
                        }
                        Some(decompressor) => {
                            if state.uncompressed_remaining == 0
                                && self.decompressed_content.len() == 0
                            {
                                // We've consumed all compressed data. It's possible for there to
                                // still be data left in the chunk that has not yet been read into
                                // `self.file_data`. This can happen when a compressor adds extra
                                // bytes after its last frame. We need to treat this as "padding
                                // after the chunk" and skip over it before reading the next record.
                                state.padding_after_compressed_data += check!(check_len(
                                    state.compressed_remaining,
                                    self.options.record_length_limit
                                )
                                .ok_or(McapError::ChunkTooLarge(state.compressed_remaining)));
                                state.compressed_remaining = 0;
                                self.currently_reading = PaddingAfterChunk;
                                continue;
                            }
                            let opcode_len_buf = decompress!(OPCODE_LEN_SIZE, state, decompressor);
                            let Some((&[opcode], rest)) = opcode_len_buf.split_first_chunk() else {
                                return Some(Err(McapError::UnexpectedEoc));
                            };
                            let Some((&len_buf, _)) = rest.split_first_chunk() else {
                                return Some(Err(McapError::UnexpectedEoc));
                            };
                            let len = u64::from_le_bytes(len_buf);
                            let len = check!(check_len(len, self.options.record_length_limit)
                                .ok_or(McapError::RecordTooLarge { opcode, len }));
                            let _ = decompress!(OPCODE_LEN_SIZE + len, state, decompressor);
                            self.decompressed_content.mark_read(OPCODE_LEN_SIZE);
                            let data = self.decompressed_content.consume(len);
                            return Some(Ok(LinearReadEvent::Record { data, opcode }));
                        }
                    }
                }
                PaddingAfterChunk => {
                    // discard any padding bytes after the chunk records and validate CRCs if
                    // necessary
                    let state = self
                        .chunk_state
                        .as_mut()
                        .expect("chunk state should be set");
                    let _ = consume!(state.padding_after_compressed_data);
                    if let Some(mut decompressor) = state.decompressor.take() {
                        check!(decompressor.reset());
                        let slot = if decompressor.name() == "lz4" { 0 } else { 1 };
                        self.decompressors[slot] = Some(decompressor);
                        if let Some(hasher) = self.decompressed_content.hasher_mut().take() {
                            let calculated = hasher.finalize();
                            let saved = state.crc;
                            if saved != 0 && saved != calculated {
                                return Some(Err(McapError::BadChunkCrc { saved, calculated }));
                            }
                        }
                    } else if let Some(hasher) = state.uncompressed_data_hasher.take() {
                        let calculated = hasher.finalize();
                        let saved = state.crc;
                        if saved != 0 && saved != calculated {
                            return Some(Err(McapError::BadChunkCrc { saved, calculated }));
                        }
                    }
                    self.chunk_state = None;
                    self.currently_reading = FileRecord;
                }
                EndMagic => {
                    if self.options.skip_end_magic {
                        return None;
                    } else {
                        let data = consume!(MAGIC.len());
                        if *data == *MAGIC {
                            self.currently_reading = AfterEndMagic;
                            continue;
                        } else {
                            return Some(Err(McapError::BadMagic));
                        }
                    }
                }
                AfterEndMagic => {
                    if self.options.check_finishes_after_end_magic {
                        if self.file_data.len() > 0 {
                            return Some(Err(McapError::BytesAfterEndMagic));
                        } else if self.at_eof {
                            return None;
                        } else {
                            return Some(Ok(LinearReadEvent::ReadRequest(1)));
                        }
                    } else {
                        return None;
                    }
                }
            }
        }
    }
}

/// Events emitted by the linear reader.
#[derive(Debug)]
pub enum LinearReadEvent<'a> {
    /// The reader needs more data to provide the next record. Call [`LinearReader::insert`] then
    /// [`LinearReader::notify_read`] to load more data. The value provided here is a hint for how
    /// much data to insert.
    ReadRequest(usize),
    /// A new record from the MCAP file. Use [`crate::parse_record`] to parse the record.
    Record { data: &'a [u8], opcode: u8 },
}

/// casts a 64-bit length from an MCAP into a [`usize`], saturating to [`usize::MAX`] if too large.
fn clamp_to_usize(len: u64) -> usize {
    len.try_into().unwrap_or(usize::MAX)
}

fn get_decompressor(
    decompressors: &mut [Option<Decoder>; 2],
    name: &str,
    budget: crate::storage::BudgetRef,
) -> McapResult<Option<Decoder>> {
    let slot = match name {
        "lz4" => 0,
        "zstd" => 1,
        "" => return Ok(None),
        _ => return Err(McapError::UnsupportedCompression(name.into())),
    };
    if let Some(decompressor) = decompressors[slot].take() {
        return Ok(Some(decompressor));
    }
    match name {
        #[cfg(feature = "zstd")]
        "zstd" => Ok(Some(Decoder::Zstd(zstd::ZstdDecoder::with_budget(budget)?))),
        #[cfg(feature = "lz4")]
        "lz4" => Ok(Some(Decoder::Lz4(lz4::Lz4Decoder::with_budget(budget)?))),
        _ => Err(McapError::UnsupportedCompression(name.into())),
    }
}

// decompresses up to `n` bytes from `from` into `to`. Repeatedly calls `decompress` until
// either the input is exhausted or enough data has been written. Returns None if all required
// data has been decompressed, or Some(need) if more bytes need to be read from the input.
fn decompress_inner(
    decompressor: &mut Decoder,
    n: usize,
    src_buf: &mut RwBuf,
    dest_buf: &mut RwBuf,
    compressed_remaining: &mut u64,
    uncompressed_remaining: &mut u64,
) -> McapResult<Option<usize>> {
    let additional = n.saturating_sub(dest_buf.len());
    if additional == 0 {
        return Ok(None);
    }
    dest_buf.reserve_exact(additional)?;
    loop {
        let need = decompressor.next_read_size();
        let have = src_buf.len();
        if need > have {
            return Ok(Some(need - have));
        }
        let dst = dest_buf.unwritten_mut();
        if dst.is_empty() {
            return Ok(None);
        }
        if *uncompressed_remaining == 0 {
            return Err(McapError::UnexpectedEoc);
        }
        let src_len = have.min(clamp_to_usize(*compressed_remaining));
        let src = &src_buf.unread()[..src_len];
        let res = decompressor.decompress(src, dst)?;
        src_buf.mark_read(res.consumed);
        dest_buf.mark_written(res.wrote);
        *compressed_remaining -= res.consumed as u64;
        *uncompressed_remaining -= res.wrote as u64;
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{parse_record, Compression};
    use std::collections::BTreeMap;
    use std::io::Read;

    const DEFAULT_LIBRARY_LENGTH: u64 = crate::LIBRARY_IDENTIFIER.len() as u64;

    fn basic_chunked_file(compression: Option<Compression>) -> McapResult<Vec<u8>> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = crate::WriteOptions::new()
                .compression(compression)
                .chunk_size(None)
                .create(&mut buf)?;
            let channel = std::sync::Arc::new(crate::Channel {
                id: 0,
                topic: "chat".to_owned(),
                schema: None,
                message_encoding: "json".to_owned(),
                metadata: BTreeMap::new(),
            });
            for n in 0..3 {
                writer.write(&crate::Message {
                    channel: channel.clone(),
                    sequence: n,
                    log_time: n as u64,
                    publish_time: n as u64,
                    data: (&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]).into(),
                })?;
                if n == 1 {
                    writer.flush()?;
                }
            }
            writer.finish()?;
        }
        Ok(buf.into_inner())
    }

    #[test]
    fn refused_buffer_growth_preserves_written_and_writable_ranges() {
        use crate::storage::{BudgetLimits, BudgetRef, ResourceCategory, StorageFailureKind};
        let domain = BudgetRef::new(BudgetLimits {
            total: BudgetRef::allocation_size() + 32768, block: 8192, retained: 0,
        }).unwrap();
        let mut buffer = rw_buf::RwBuf::with_budget(false, domain.clone(), ResourceCategory::Input);
        buffer.tail_with_size(128).unwrap().fill(7);
        buffer.mark_written(64);
        let before = domain.workload_statistics().current;
        assert_eq!(buffer.reserve_exact(8192).unwrap_err().kind, StorageFailureKind::PermanentLimit);
        assert_eq!(buffer.unread(), &[7; 64]);
        assert_eq!(buffer.unwritten(), &[7; 64]);
        assert_eq!(buffer.reserve_exact(usize::MAX).unwrap_err().kind, StorageFailureKind::Overflow);
        assert_eq!(buffer.unwritten(), &[7; 64]);
        assert_eq!(domain.workload_statistics().current, before);
        buffer.mark_written(64);
        assert_eq!(buffer.unread(), &[7; 128]);
        drop(buffer);
        assert_eq!(domain.workload_statistics().current, 0);
    }

    #[test]
    fn consumed_leased_input_releases_parser_pin_before_capacity_retry() {
        use crate::storage::{BudgetLimits, OwnerKind, ResourceCategory};
        let domain=crate::storage::BudgetRef::new(BudgetLimits {total:(12000) + crate::storage::BudgetRef::allocation_size(),block:8192,retained:0}).unwrap();
        let mut buffer=rw_buf::RwBuf::with_budget(false,domain.clone(),ResourceCategory::Input);
        let data=buffer.tail_with_size(8192).unwrap();
        data.fill(7);
        let pointer=data.as_ptr();
        buffer.mark_written(8192);
        let mut lease=buffer.share(pointer,8192).unwrap();
        lease.set_owner(OwnerKind::Lease);
        buffer.mark_read(8192);
        assert!(buffer.tail_with_size(4096).is_err());
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],0);
        assert!(domain.ownership_statistics().externally_releasable>8192);
        assert_eq!(lease.as_ref(), &[7;8192]);
        drop(lease);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(buffer.tail_with_size(4096).unwrap().len(),4096);
        drop(buffer);
        assert_eq!(domain.workload_statistics().current,0);
    }
    #[test]
    fn memory_domain_freezes_after_allocation_or_progress() {


        let original = crate::storage::BudgetRef::new(crate::storage::BudgetLimits {
            retained: 0,
            ..Default::default()
        }).unwrap();
        let replacement = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut reader = LinearReader::new();
        reader.set_memory_budget(original.clone()).unwrap();
        reader.try_insert(128).unwrap();
        let charged = original.workload_statistics().current;
        assert!(charged > 0);
        assert!(reader.set_memory_budget(replacement.clone()).is_err());
        reader.set_memory_budget(original.clone()).unwrap();
        assert_eq!(original.workload_statistics().current, charged);
        assert_eq!(replacement.workload_statistics().current, 0);
        drop(reader);
        assert_eq!(original.workload_statistics().current, 0);
        let mut reader = LinearReader::new();
        reader.set_memory_budget(original.clone()).unwrap();
        let _ = reader.next_event();
        assert!(reader.set_memory_budget(replacement).is_err());
        reader.set_memory_budget(original).unwrap();
    }

    #[test]
    fn chunk_codec_uses_selected_domain_from_construction() {


        for compression in ["lz4", "zstd"] {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let header = ChunkHeader {
                message_start_time: 0,
                message_end_time: 0,
                uncompressed_size: 0,
                uncompressed_crc: 0,
                compression: compression.into(),
                compressed_size: 0,
            };
            let mut reader = LinearReader::for_chunk_with_budget(header, domain.clone()).unwrap();
            assert!(domain.workload_statistics().current > 0);
            reader.set_memory_budget(domain.clone()).unwrap();
            assert!(reader
                .set_memory_budget(crate::storage::BudgetRef::new(Default::default()).unwrap())
                .is_err());
            drop(reader);
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }

    #[test]
    fn test_un_chunked() -> McapResult<()> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = crate::WriteOptions::new()
                .use_chunks(false)
                .create(&mut buf)?;
            let channel = std::sync::Arc::new(crate::Channel {
                id: 0,
                topic: "chat".to_owned(),
                schema: None,
                message_encoding: "json".to_owned(),
                metadata: BTreeMap::new(),
            });
            writer.write(&crate::Message {
                channel,
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: (&[0, 1, 2]).into(),
            })?;
            writer.finish()?;
        }
        let mut reader = LinearReader::new();
        let mut cursor = std::io::Cursor::new(buf.into_inner());
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        while let Some(event) = reader.next_event() {
            match event? {
                LinearReadEvent::ReadRequest(n) => {
                    let written = cursor.read(reader.insert(n))?;
                    reader.notify_read(written);
                }
                LinearReadEvent::Record { data, opcode } => {
                    opcodes.push(opcode);
                    parse_record(opcode, data)?;
                }
            }
            assert!(iter_count < 10000);
            iter_count += 1;
        }
        assert_eq!(
            opcodes,
            vec![
                op::HEADER,
                op::CHANNEL,
                op::MESSAGE,
                op::DATA_END,
                op::CHANNEL,
                op::STATISTICS,
                op::SUMMARY_OFFSET,
                op::SUMMARY_OFFSET,
                op::FOOTER
            ]
        );

        Ok(())
    }

    #[test]
    fn test_file_data_validation() -> McapResult<()> {
        let mut reader = LinearReader::new_with_options(
            LinearReaderOptions::default()
                .with_validate_data_section_crc(true)
                .with_validate_summary_section_crc(true),
        );
        let mut cursor = std::io::Cursor::new(basic_chunked_file(None)?);
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        while let Some(event) = reader.next_event() {
            match event? {
                LinearReadEvent::ReadRequest(n) => {
                    let written = cursor.read(reader.insert(n))?;
                    reader.notify_read(written);
                }
                LinearReadEvent::Record { data, opcode } => {
                    opcodes.push(opcode);
                    parse_record(opcode, data)?;
                }
            }
            iter_count += 1;
            // guard against infinite loop
            assert!(iter_count < 10000);
        }
        Ok(())
    }

    #[test]
    fn test_record_length_limit() {
        let mut reader = LinearReader::new_with_options(
            LinearReaderOptions::default().with_record_length_limit(10),
        );
        let mut cursor = std::io::Cursor::new(basic_chunked_file(None).unwrap());
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        while let Some(event) = reader.next_event() {
            match event {
                Ok(LinearReadEvent::ReadRequest(n)) => {
                    let written = cursor
                        .read(reader.insert(n))
                        .expect("insert should not fail");
                    reader.notify_read(written);
                }
                Ok(LinearReadEvent::Record { data, opcode }) => {
                    opcodes.push(opcode);
                    parse_record(opcode, data).expect("parse should not fail");
                }
                Err(err) => {
                    match err {
                        McapError::RecordTooLarge { opcode, len } => {
                            assert_eq!(opcode, op::HEADER);
                            assert_eq!(len, 8 + DEFAULT_LIBRARY_LENGTH);
                        }
                        _ => panic!("expected RecordTooLarge, got {err:?}"),
                    }
                    return;
                }
            }
            iter_count += 1;
            // guard against infinite loop
            assert!(iter_count < 10000);
        }
        panic!("should have errored")
    }

    fn test_chunked(
        compression: Option<Compression>,
        options: LinearReaderOptions,
    ) -> McapResult<()> {
        let mut reader = LinearReader::new_with_options(options);
        let mut cursor = std::io::Cursor::new(basic_chunked_file(compression)?);
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        while let Some(event) = reader.next_event() {
            match event? {
                LinearReadEvent::ReadRequest(n) => {
                    let written = cursor.read(reader.insert(n))?;
                    reader.notify_read(written);
                }
                LinearReadEvent::Record { data, opcode } => {
                    opcodes.push(opcode);
                    parse_record(opcode, data)?;
                }
            }
            iter_count += 1;
            // guard against infinite loop
            assert!(iter_count < 10000);
        }
        assert_eq!(
            opcodes,
            vec![
                op::HEADER,
                op::CHANNEL,
                op::MESSAGE,
                op::MESSAGE,
                op::MESSAGE_INDEX,
                op::MESSAGE,
                op::MESSAGE_INDEX,
                op::DATA_END,
                op::CHANNEL,
                op::STATISTICS,
                op::CHUNK_INDEX,
                op::CHUNK_INDEX,
                op::SUMMARY_OFFSET,
                op::SUMMARY_OFFSET,
                op::SUMMARY_OFFSET,
                op::FOOTER
            ]
        );
        Ok(())
    }
    use assert_matches::assert_matches;
    use paste::paste;

    macro_rules! test_chunked_parametrized {
        ($($name:ident, $compression:expr, $options:expr),*) => {
            $(
                paste! {
                    #[test]
                    fn [ <test_chunked_ $name> ]() -> McapResult<()> {
                        test_chunked($compression, $options)
                    }
                }
            )*

        };
    }

    test_chunked_parametrized! {
        none_none, None, LinearReaderOptions::default(),
        none_after, None, LinearReaderOptions::default().with_validate_chunk_crcs(true),
        none_before, None, LinearReaderOptions::default().with_prevalidate_chunk_crcs(true),
        zstd_none, Some(Compression::Zstd), LinearReaderOptions::default(),
        zstd_after, Some(Compression::Zstd), LinearReaderOptions::default().with_validate_chunk_crcs(true),
        zstd_before, Some(Compression::Zstd), LinearReaderOptions::default().with_prevalidate_chunk_crcs(true),
        lz4_none, Some(Compression::Lz4), LinearReaderOptions::default(),
        lz4_after, Some(Compression::Lz4), LinearReaderOptions::default().with_validate_chunk_crcs(true),
        lz4_before, Some(Compression::Lz4), LinearReaderOptions::default().with_prevalidate_chunk_crcs(true)
    }

    #[test]
    fn test_no_magic() -> McapResult<()> {
        for options in [
            LinearReaderOptions::default().with_skip_start_magic(true),
            LinearReaderOptions::default().with_skip_end_magic(true),
        ] {
            let mcap = basic_chunked_file(None)?;
            let input = if options.skip_start_magic {
                &mcap[8..]
            } else if options.skip_end_magic {
                &mcap[..mcap.len() - 8]
            } else {
                panic!("options should either skip start or end magic")
            };
            let mut reader = LinearReader::new_with_options(options);
            let mut cursor = std::io::Cursor::new(input);
            let mut opcodes: Vec<u8> = Vec::new();
            let mut iter_count = 0;
            while let Some(event) = reader.next_event() {
                match event? {
                    LinearReadEvent::ReadRequest(n) => {
                        let written = cursor.read(reader.insert(n))?;
                        reader.notify_read(written);
                    }
                    LinearReadEvent::Record { data, opcode } => {
                        opcodes.push(opcode);
                        parse_record(opcode, data)?;
                    }
                }
                iter_count += 1;
                // guard against infinite loop
                assert!(iter_count < 10000);
            }
            assert_eq!(
                opcodes,
                vec![
                    op::HEADER,
                    op::CHANNEL,
                    op::MESSAGE,
                    op::MESSAGE,
                    op::MESSAGE_INDEX,
                    op::MESSAGE,
                    op::MESSAGE_INDEX,
                    op::DATA_END,
                    op::CHANNEL,
                    op::STATISTICS,
                    op::CHUNK_INDEX,
                    op::CHUNK_INDEX,
                    op::SUMMARY_OFFSET,
                    op::SUMMARY_OFFSET,
                    op::SUMMARY_OFFSET,
                    op::FOOTER
                ]
            );
        }
        Ok(())
    }

    #[test]
    fn test_emit_chunks() -> McapResult<()> {
        let mcap = basic_chunked_file(None)?;
        let mut reader =
            LinearReader::new_with_options(LinearReaderOptions::default().with_emit_chunks(true));
        let mut cursor = std::io::Cursor::new(mcap);
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        while let Some(event) = reader.next_event() {
            match event? {
                LinearReadEvent::ReadRequest(n) => {
                    let written = cursor.read(reader.insert(n))?;
                    reader.notify_read(written);
                }
                LinearReadEvent::Record { data, opcode } => {
                    opcodes.push(opcode);
                    parse_record(opcode, data)?;
                }
            }
            iter_count += 1;
            // guard against infinite loop
            assert!(iter_count < 10000);
        }
        assert_eq!(
            opcodes,
            vec![
                op::HEADER,
                op::CHUNK,
                op::MESSAGE_INDEX,
                op::CHUNK,
                op::MESSAGE_INDEX,
                op::DATA_END,
                op::CHANNEL,
                op::STATISTICS,
                op::CHUNK_INDEX,
                op::CHUNK_INDEX,
                op::SUMMARY_OFFSET,
                op::SUMMARY_OFFSET,
                op::SUMMARY_OFFSET,
                op::FOOTER
            ]
        );
        Ok(())
    }

    // Ensures that the internal buffer for the linear reader gets compacted regularly and does not
    // expand unbounded.
    #[test]
    fn test_buffer_compevent() -> McapResult<()> {
        let mut buf = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(buf);
            let data = Vec::from_iter(std::iter::repeat_n(0x20u8, 1024 * 1024 * 4));
            let mut writer = crate::WriteOptions::new()
                .compression(None)
                .chunk_size(None)
                .create(&mut cursor)?;
            let channel = std::sync::Arc::new(crate::Channel {
                id: 0,
                topic: "chat".to_owned(),
                schema: None,
                message_encoding: "json".to_owned(),
                metadata: BTreeMap::new(),
            });
            for n in 0..3 {
                writer.write(&crate::Message {
                    channel: channel.clone(),
                    sequence: n,
                    log_time: n as u64,
                    publish_time: n as u64,
                    data: std::borrow::Cow::Borrowed(&data[..]),
                })?;
                if n == 1 {
                    writer.flush()?;
                }
            }
            writer.finish()?;
            drop(writer);
            buf = cursor.into_inner();
        }
        let mut reader = LinearReader::new();
        let mut cursor = std::io::Cursor::new(buf);
        let mut opcodes: Vec<u8> = Vec::new();
        let mut iter_count = 0;
        let mut max_needed: usize = 0;
        while let Some(event) = reader.next_event() {
            match event? {
                LinearReadEvent::ReadRequest(n) => {
                    max_needed = std::cmp::max(max_needed, n);
                    // read slightly more than requested, such that the data in the buffer does not
                    // hit zero after the next event.
                    let written = cursor.read(reader.insert(n + 1))?;
                    reader.notify_read(written);
                    let buffer_size = reader.file_data.buffer().len();
                    assert!(
                        // The shared pool rounds small requests up to its 4 KiB class.
                        buffer_size <= std::cmp::max(max_needed * 2, 4096),
                        "max needed: {max_needed}, buffer size: {buffer_size}",
                    );
                }
                LinearReadEvent::Record { data, opcode } => {
                    opcodes.push(opcode);
                    parse_record(opcode, data)?;
                }
            }
            iter_count += 1;
            // guard against infinite loop
            assert!(iter_count < 10000);
        }
        Ok(())
    }

    #[test]
    fn test_decompression_does_not_fail() {
        let mut f = std::fs::File::open("tests/data/zstd_chunk_with_padding.mcap")
            .expect("failed to open file");
        let blocksize: usize = 1024;
        let mut reader = LinearReader::new();
        let mut message_count = 0;
        while let Some(event) = reader.next_event() {
            match event.expect("failed to get next event") {
                LinearReadEvent::Record { opcode, .. } => {
                    if opcode == op::MESSAGE {
                        message_count += 1;
                    }
                }
                LinearReadEvent::ReadRequest(_) => {
                    let read = f
                        .read(reader.insert(blocksize))
                        .expect("failed to read from file");
                    reader.notify_read(read);
                }
            }
        }
        assert_eq!(message_count, 12);
    }

    #[test]
    fn test_handles_failed_to_close_chunks() {
        let mut f =
            std::fs::File::open("tests/data/chunk_not_closed.mcap").expect("failed to open file");
        let mut output = vec![];
        f.read_to_end(&mut output).expect("failed to read");

        let mut reader = LinearReader::new();
        reader.insert(output.len()).copy_from_slice(&output[..]);
        reader.notify_read(output.len());

        // the first record is the header;
        let next = reader
            .next_event()
            .expect("there should be one event")
            .expect("first record should be header");

        let LinearReadEvent::Record { opcode: 1, .. } = next else {
            panic!("expected first record to be header");
        };

        let next = reader.next_event().expect("there should be one event");

        // test fails with unexpected EOC because sizes are u64::max
        assert_matches!(next, Err(McapError::UnexpectedEoc));
    }

    #[test]
    fn test_notifying_eof_after_writing_whole_file() {
        let mcap = basic_chunked_file(None).unwrap();
        let mut reader = LinearReader::new();
        reader.insert(mcap.len()).copy_from_slice(mcap.as_slice());
        reader.notify_read(mcap.len());
        while let Some(event) = reader.next_event() {
            match event.unwrap() {
                LinearReadEvent::ReadRequest(_) => {
                    panic!("should not request read because file is complete");
                }
                LinearReadEvent::Record { .. } => {}
            }
        }
        reader.notify_read(0);
        assert_matches!(reader.next_event(), None);
    }

    #[test]
    fn test_trailing_garbage_after_end_magic() {
        let mcap = basic_chunked_file(None).unwrap();
        let mut reader = LinearReader::new_with_options(
            LinearReaderOptions::default().with_check_finishes_after_end_magic(true),
        );
        reader.insert(mcap.len()).copy_from_slice(mcap.as_slice());
        reader.notify_read(mcap.len());
        while let Some(event) = reader.next_event() {
            match event.unwrap() {
                LinearReadEvent::ReadRequest(_) => break,
                LinearReadEvent::Record { .. } => {}
            }
        }
        assert_matches!(
            reader.next_event(),
            Some(Ok(LinearReadEvent::ReadRequest(_)))
        );
        let garbage = b"garbage";
        reader.insert(garbage.len()).copy_from_slice(garbage);
        reader.notify_read(garbage.len());
        assert_matches!(
            reader.next_event(),
            Some(Err(McapError::BytesAfterEndMagic))
        );
    }
}

/// Owned counterparts to the borrowed parser events.
pub enum SharedReadEvent {
    ReadRequest(usize),
    Record {
        opcode: u8,
        data: crate::storage::SharedBytes,
    },
}
