//! Write MCAP files

#[cfg(test)]
use std::io::Cursor;
#[cfg(test)]
use crate::Channel;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::{self, prelude::*, SeekFrom},
    mem::size_of,
};

use crate::canonical::CanonicalMap;
use binrw::prelude::*;
use byteorder::{WriteBytesExt, LE};
use enumset::{EnumSet, EnumSetType};

use crate::{
    chunk_sink::{ChunkMode, ChunkSink},
    io_utils::{CountingCrcWriter, McapWrite},
    records::{self, op, AttachmentHeader, MessageHeader, Record},
    Attachment, Compression, McapError, McapResult, Message, Summary, MAGIC,
};

// re-export to help with linear writing
pub use binrw::io::NoSeek;

pub use records::Metadata;

enum WriteMode<W: Write + Seek> {
    Raw(CountingCrcWriter<W>),
    Chunk(ChunkWriter<W>),
    Attachment(AttachmentWriter<CountingCrcWriter<W>>),
    Failed(W),
}

#[derive(EnumSetType, Debug)]
pub enum PrivateRecordOptions {
    /// If set and chunking is enabled, the private record will be written into a chunk. Otherwise, the record will be written directly to the file.
    IncludeInChunks,
}

fn op_and_len<W: Write>(w: &mut W, op: u8, len: u64) -> io::Result<()> {
    w.write_u8(op)?;
    w.write_u64::<LE>(len)?;
    Ok(())
}

fn serialized_body_len<T>(body: &T) -> io::Result<u64>
where
    T: BinWrite,
    for<'a> T::Args<'a>: Default,
{
    let mut counter = NoSeek::new(io::sink());
    counter.write_le(body).map_err(io::Error::other)?;
    counter.stream_position()
}

fn write_record_body<W: Write, T>(writer: &mut W, opcode: u8, body: &T) -> io::Result<()>
where
    T: BinWrite,
    for<'a> T::Args<'a>: Default,
{
    op_and_len(writer, opcode, serialized_body_len(body)?)?;
    NoSeek::new(writer).write_le(body).map_err(io::Error::other)
}

fn write_schema_record<W: Write, T>(mut writer: &mut W, header: &T, data: &[u8]) -> io::Result<()>
where
    T: BinWrite,
    for<'a> T::Args<'a>: Default,
{
    let header_len = serialized_body_len(header)?;
    op_and_len(
        writer,
        op::SCHEMA,
        header_len + size_of::<u32>() as u64 + data.len() as u64,
    )?;
    NoSeek::new(&mut writer)
        .write_le(header)
        .map_err(io::Error::other)?;
    writer.write_u32::<LE>(data.len() as u32)?;
    writer.write_all(data)
}

fn write_record<W: Write>(mut w: &mut W, r: &Record) -> io::Result<()> {
    // Official record serializers only query their position. Measure into a sink,
    // then serialize directly, avoiding an intermediate allocation and copy.
    macro_rules! record {
        ($op:expr, $b:ident) => {{
            write_record_body(w, $op, $b)?;
        }};
    }

    match r {
        Record::Header(h) => record!(op::HEADER, h),
        Record::Footer(_) => {
            unreachable!("Footer handles its own serialization because its CRC is self-referencing")
        }
        Record::Schema { header, data } => write_schema_record(w, header, data)?,
        Record::Channel(c) => record!(op::CHANNEL, c),
        Record::Message { header, data } => {
            let header_len = header.serialized_len();
            op_and_len(w, op::MESSAGE, header_len + data.len() as u64)?;
            NoSeek::new(&mut w)
                .write_le(header)
                .map_err(io::Error::other)?;
            w.write_all(data)?;
        }
        Record::Chunk { .. } => {
            unreachable!("Chunks handle their own serialization due to seeking shenanigans")
        }
        Record::MessageIndex(_) => {
            unreachable!("MessageIndexes handle their own serialization to recycle the buffer between indexes")
        }
        Record::ChunkIndex(c) => record!(op::CHUNK_INDEX, c),
        Record::Attachment { .. } => {
            unreachable!("Attachments handle their own serialization to handle large files")
        }
        Record::AttachmentIndex(ai) => record!(op::ATTACHMENT_INDEX, ai),
        Record::Statistics(s) => record!(op::STATISTICS, s),
        Record::Metadata(m) => record!(op::METADATA, m),
        Record::MetadataIndex(mi) => record!(op::METADATA_INDEX, mi),
        Record::SummaryOffset(so) => record!(op::SUMMARY_OFFSET, so),
        Record::DataEnd(eod) => record!(op::DATA_END, eod),
        Record::Unknown { opcode, data } => {
            let len = data.len();
            let op = *opcode;
            op_and_len(w, op, len as u64)?;
            w.write_all(data)?;
        }
    };
    Ok(())
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    memory_budget: Option<crate::storage::BudgetRef>,
    compression: Option<Compression>,
    profile: crate::option_text::Text,
    library: crate::option_text::Text,
    chunk_size: Option<u64>,
    use_chunks: bool,
    disable_seeking: bool,
    emit_statistics: bool,
    emit_summary_offsets: bool,
    emit_message_indexes: bool,
    emit_chunk_indexes: bool,
    emit_attachment_indexes: bool,
    emit_metadata_indexes: bool,
    repeat_channels: bool,
    repeat_schemas: bool,
    calculate_chunk_crcs: bool,
    calculate_data_section_crc: bool,
    calculate_summary_section_crc: bool,
    calculate_attachment_crcs: bool,
    #[cfg(any(feature = "zstd", feature = "lz4"))]
    compression_level: u32,
    #[cfg(feature = "zstd")]
    compression_threads: Option<u32>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            memory_budget: Default::default(),
            #[cfg(feature = "zstd")]
            compression: Some(Compression::Zstd),
            #[cfg(not(feature = "zstd"))]
            compression: None,
            profile: crate::option_text::Text::Borrowed(""),
            library: crate::option_text::Text::Borrowed(crate::LIBRARY_IDENTIFIER),
            chunk_size: Some(Self::DEFAULT_CHUNK_SIZE),
            use_chunks: true,
            disable_seeking: false,
            emit_statistics: true,
            emit_summary_offsets: true,
            emit_message_indexes: true,
            emit_chunk_indexes: true,
            emit_attachment_indexes: true,
            emit_metadata_indexes: true,
            repeat_channels: true,
            repeat_schemas: true,
            calculate_chunk_crcs: true,
            calculate_data_section_crc: true,
            calculate_summary_section_crc: true,
            calculate_attachment_crcs: true,
            #[cfg(any(feature = "zstd", feature = "lz4"))]
            compression_level: 0,
            #[cfg(feature = "zstd")]
            compression_threads: None,
        }
    }
}

impl WriteOptions {
    /// Selects the shared native storage domain.
    pub fn memory_budget(mut self, budget: crate::storage::BudgetRef) -> Self {
        self.memory_budget = Some(budget);
        self
    }
    /// Default target uncompressed chunk size used by [`WriteOptions`].
    pub const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;

    pub fn new() -> Self {
        Self::default()
    }

    /// Specifies the compression that should be used on chunks.
    pub fn compression(self, compression: Option<Compression>) -> Self {
        Self {
            compression,
            ..self
        }
    }

    /// Specifies the profile that should be written to the MCAP Header record.
    pub fn profile<S: Into<String>>(self, profile: S) -> Self {
        Self {
            profile: crate::option_text::Text::Owned(profile.into()),
            ..self
        }
    }

    /// Specifies the library that should be written to the MCAP Header record.
    ///
    /// This is a free-form string that can be used to identify the library that wrote the file.
    /// It is not used for any other purpose.
    pub fn library<S: Into<String>>(self, library: S) -> Self {
        Self {
            library: crate::option_text::Text::Owned(library.into()),
            ..self
        }
    }

    /// Retain profile text in the selected storage domain; clones share its allocation.
    pub fn try_profile(mut self, profile: &str) -> McapResult<Self> {
        let domain=self.text_domain()?;
        self.profile=crate::option_text::Text::new(profile,&domain)?;
        Ok(self)
    }
    /// Retain library text in the selected storage domain; clones share its allocation.
    pub fn try_library(mut self, library: &str) -> McapResult<Self> {
        let domain=self.text_domain()?;
        self.library=crate::option_text::Text::new(library,&domain)?;
        Ok(self)
    }
    fn text_domain(&mut self)->McapResult<crate::storage::BudgetRef> {
        let domain=match &self.memory_budget {Some(domain)=>domain.clone(),None=>crate::storage::BudgetRef::try_default()?};
        self.check_text_domain(&domain)?;
        self.memory_budget=Some(domain.clone());
        Ok(domain)
    }
    fn check_text_domain(&self,domain:&crate::storage::BudgetRef)->McapResult<()> {
        if !self.profile.belongs_to(domain) || !self.library.belongs_to(domain) {
            return Err(McapError::StaticIoError("Writer option storage cannot change memory budget domains"));
        }
        Ok(())
    }

    /// Specifies the target uncompressed size of each chunk.
    ///
    /// Messages will be written to chunks until the uncompressed chunk is larger than the
    /// target chunk size, at which point the chunk will be closed and a new one started.
    /// If `None`, chunks will not be automatically closed and the user must call `flush()` to
    /// begin a new chunk.
    pub fn chunk_size(self, chunk_size: Option<u64>) -> Self {
        Self { chunk_size, ..self }
    }

    /// Specifies whether to use chunks for storing messages.
    ///
    /// If `false`, messages will be written directly to the data section of the file.
    /// This prevents using compression or indexing, but may be useful on small embedded systems
    /// that cannot afford the memory overhead of storing chunk metadata for the entire recording.
    ///
    /// Note that it's often useful to post-process a non-chunked file using `mcap recover` to add
    /// indexes for efficient processing.
    pub fn use_chunks(self, use_chunks: bool) -> Self {
        Self { use_chunks, ..self }
    }

    /// Specifies whether the writer should seek or not.
    ///
    /// Setting `true` will allow you to use [`NoSeek`] on the destination writer to support
    /// writing to a stream that does not support [`Seek`].
    ///
    /// By default the writer will seek the output to avoid buffering in memory. Seeking is an
    /// optimization and should only be disabled if the output is using [`NoSeek`].
    pub fn disable_seeking(mut self, disable_seeking: bool) -> Self {
        self.disable_seeking = disable_seeking;
        self
    }

    /// Specifies in whether to write any records to the [summary
    /// section](https://mcap.dev/spec#summary-section).
    ///
    /// If you want only want to include specific record types in the summary section, call this
    /// method with `false` and then enable the records you want. This ensures that no unwanted
    /// summary records will be written if the format changes in the future.
    ///
    /// Note that this does *not* control whether [summary offset
    /// records](https://mcap.dev/spec#summary-offset-op0x0e) are written, because they
    /// are not part of the [summary section](https://mcap.dev/spec#summary-section).
    pub fn emit_summary_records(mut self, value: bool) -> Self {
        self.emit_statistics = value;
        self.emit_chunk_indexes = value;
        self.emit_attachment_indexes = value;
        self.emit_metadata_indexes = value;
        self.repeat_channels = value;
        self.repeat_schemas = value;
        self
    }

    /// Specifies whether to write [summary offset
    /// records](https://mcap.dev/spec#summary-offset-op0x0e). This is on by default.
    pub fn emit_summary_offsets(mut self, emit_summary_offsets: bool) -> Self {
        self.emit_summary_offsets = emit_summary_offsets;
        self
    }

    /// Specifies whether to write a [statistics record](https://mcap.dev/spec#statistics-op0x0b) in
    /// the [summary section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn emit_statistics(mut self, emit_statistics: bool) -> Self {
        self.emit_statistics = emit_statistics;
        self
    }

    /// Specifies whether to write [message index
    /// records](https://mcap.dev/spec#message-index-op0x07) after each chunk. This is on by
    /// default.
    pub fn emit_message_indexes(mut self, emit_message_indexes: bool) -> Self {
        self.emit_message_indexes = emit_message_indexes;
        self
    }

    /// Specifies whether to write [chunk index records](https://mcap.dev/spec#chunk-index-op0x08)
    /// in the [summary section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn emit_chunk_indexes(mut self, emit_chunk_indexes: bool) -> Self {
        self.emit_chunk_indexes = emit_chunk_indexes;
        self
    }

    /// Specifies whether to write [attachment index
    /// records](https://mcap.dev/spec#attachment-index-op0x0a) in the [summary
    /// section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn emit_attachment_indexes(mut self, emit_attachment_indexes: bool) -> Self {
        self.emit_attachment_indexes = emit_attachment_indexes;
        self
    }

    /// Specifies whether to write [metadata index
    /// records](https://mcap.dev/spec#metadata-index-op0x0d) in the [summary
    /// section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn emit_metadata_indexes(mut self, emit_metadata_indexes: bool) -> Self {
        self.emit_metadata_indexes = emit_metadata_indexes;
        self
    }

    /// Specifies whether to repeat each [channel record](https://mcap.dev/spec#channel-op0x04) from
    /// the [data section](https://mcap.dev/spec#data-section) in the [summary
    /// section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn repeat_channels(mut self, repeat_channels: bool) -> Self {
        self.repeat_channels = repeat_channels;
        self
    }

    /// Specifies whether to repeat each [schema record](https://mcap.dev/spec#schema-op0x03) from
    /// the [data section](https://mcap.dev/spec#data-section) in the [summary
    /// section](https://mcap.dev/spec#summary-section). This is on by default.
    pub fn repeat_schemas(mut self, repeat_schemas: bool) -> Self {
        self.repeat_schemas = repeat_schemas;
        self
    }

    /// Specifies the compression level to use. A value of zero instructs the
    /// compressor to use the default compression level.
    #[cfg(any(feature = "zstd", feature = "lz4"))]
    pub fn compression_level(mut self, compression_level: u32) -> Self {
        self.compression_level = compression_level;
        self
    }

    /// Specifies how many threads to use for compression. A value of zero
    /// disables multithreaded compression. The default number of threads
    /// is equal to the number of physical CPUs.
    #[cfg(feature = "zstd")]
    pub fn compression_threads(mut self, compression_threads: u32) -> Self {
        self.compression_threads = Some(compression_threads);
        self
    }

    /// Creates a [`Writer`] which writes to `w` using the given options
    pub fn create<W: Write + Seek>(self, w: W) -> McapResult<Writer<W>> {
        Writer::with_options(w, self)
    }

    /// Specifies whether to calculate and write CRCs for chunk records. This is on by default.
    pub fn calculate_chunk_crcs(mut self, calculate_chunk_crcs: bool) -> Self {
        self.calculate_chunk_crcs = calculate_chunk_crcs;
        self
    }

    /// Specifies whether to calculate and write a data section CRC into the DataEnd record. This is on by default.
    pub fn calculate_data_section_crc(mut self, calculate_data_section_crc: bool) -> Self {
        self.calculate_data_section_crc = calculate_data_section_crc;
        self
    }

    /// Specifies whether to calculate and write a summary section CRC into the Footer record. This is on by default.
    pub fn calculate_summary_section_crc(mut self, calculate_summary_section_crc: bool) -> Self {
        self.calculate_summary_section_crc = calculate_summary_section_crc;
        self
    }

    /// Specifies whether to calculate and write a CRC for attachments. This is on by default.
    pub fn calculate_attachment_crcs(mut self, calculate_attachment_crcs: bool) -> Self {
        self.calculate_attachment_crcs = calculate_attachment_crcs;
        self
    }
}

fn validate_channel_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> io::Result<()> {
    let mut previous = None;
    for (key, _) in pairs {
        if previous.is_some_and(|p| p >= key) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        previous = Some(key);
    }
    Ok(())
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct ChannelContent<'a, I> {
    topic: Cow<'a, str>,
    schema_id: u16,
    message_encoding: Cow<'a, str>,
    metadata: I,
}

impl<'a, I: Clone + Iterator<Item = (&'a str, &'a str)>> ChannelContent<'a, I> {
    fn into_owned(
        self,
        domain: &crate::storage::BudgetRef,
    ) -> Result<crate::declaration_key::ChannelKey,crate::storage::StorageFailure> {
        crate::declaration_key::ChannelKey::new_pairs(
            &self.topic,
            self.schema_id,
            &self.message_encoding,
            self.metadata.clone(),
            domain,
        )
    }
    fn compare(&self, stored: &crate::declaration_key::ChannelKey) -> std::cmp::Ordering {
        stored.compare_pairs(
            &self.topic,
            self.schema_id,
            &self.message_encoding,
            self.metadata.clone(),
        )
    }
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct SchemaContent<'a> {
    name: Cow<'a, str>,
    encoding: Cow<'a, str>,
    data: Cow<'a, [u8]>,
}

impl SchemaContent<'_> {
    fn into_owned(
        self,
        domain: &crate::storage::BudgetRef,
    ) -> Result<crate::declaration_key::SchemaKey,crate::storage::StorageFailure> {
        crate::declaration_key::SchemaKey::new(&self.name, &self.encoding, &self.data, domain)
    }
    fn compare(&self, stored: &crate::declaration_key::SchemaKey) -> std::cmp::Ordering {
        stored.compare(&self.name, &self.encoding, &self.data)
    }
}

/// Writes an MCAP file to the given [writer](Write).
///
/// Users should call [`finish()`](Self::finish) to flush the stream
/// and check for errors when done; otherwise the result will be unwrapped on drop.
pub struct Writer<W: Write + Seek> {
    domain: crate::storage::BudgetRef,
    bookkeeping: crate::storage::Bookkeeping,
    writer: Option<WriteMode<W>>,
    finished_summary: Option<Summary>,
    chunk_mode: ChunkMode,
    options: WriteOptions,
    // Maps all unique channel content to its "canonical" or first written ID.
    canonical_channels: CanonicalMap<crate::declaration_key::ChannelKey>,
    // Maps all written IDs of channels to the canonical ID for their content.
    all_channel_ids: crate::u16_table::U16Table<u16>,
    // Maps all unique schema content to its "canonical" or first written ID.
    canonical_schemas: CanonicalMap<crate::declaration_key::SchemaKey>,
    // Maps all written IDs of schemas to the canonical ID for their content.
    all_schema_ids: crate::u16_table::U16Table<u16>,
    next_schema_id: u16,
    next_channel_id: u16,
    chunk_indexes: crate::segmented::SharedSegmentedVec<crate::shared_chunk_index::SharedChunkIndex>,
    attachment_count: u32,
    attachment_indexes:
        crate::segmented::SharedSegmentedVec<crate::shared_attachment_index::SharedAttachmentIndex>,
    metadata_count: u32,
    metadata_indexes:
        crate::segmented::SharedSegmentedVec<crate::shared_metadata_index::SharedMetadataIndex>,
    /// Message start and end time, or None if there are no messages yet.
    message_bounds: Option<(u64, u64)>,
    channel_message_counts: crate::u16_table::SharedU16Table<u64>,
}

impl<W: Write + Seek> Writer<W> {
    /// Create a new MCAP [`Writer`] using the provided seeking writer.
    pub fn new(writer: W) -> McapResult<Self> {
        Self::with_options(writer, WriteOptions::default())
    }

    /// Create a new MCAP [`Writer`] using the provided seeking writer and [`WriteOptions`].
    pub fn with_options(writer: W, mut opts: WriteOptions) -> McapResult<Self> {
        let domain = match opts.memory_budget.clone() {
            Some(domain) => domain,
            None => crate::storage::BudgetRef::try_default()?,
        };
        opts.check_text_domain(&domain)?;
        #[cfg(feature = "zstd")]
        if opts.compression_threads.is_none() {
            opts.compression_threads = Some(if opts.use_chunks && matches!(opts.compression, Some(Compression::Zstd)) {
                num_cpus::get_physical() as u32
            } else { 0 });
        }
        let mut writer = CountingCrcWriter::new(writer, opts.calculate_data_section_crc);
        writer.write_all(MAGIC)?;

        write_record_body(
            &mut writer,
            op::HEADER,
            &records::HeaderRef {
                profile: &opts.profile,
                library: &opts.library,
            },
        )?;

        // If both the `use_chunks` and `disable_seeking` options are enabled set the chunk
        // mode and pre-allocate the buffer. Checking both avoids allocating the temporary buffer
        // if seeking is disabled but chunking is not.
        let chunk_mode = if opts.use_chunks && opts.disable_seeking {
            let buffer_size = opts.chunk_size.unwrap_or_default();

            let size: usize = buffer_size
                .try_into()
                .map_err(|_| McapError::ChunkBufferTooLarge(buffer_size))?;
            if size > domain.limits().block {
                return Err(McapError::ChunkBufferTooLarge(buffer_size));
            }
            let (buffer, charge) = crate::charged::bytes(
                &domain,
                crate::storage::ResourceCategory::Writer,
                size,
            )?;
            ChunkMode::Buffered { buffer, charge }
        } else {
            ChunkMode::Direct
        };

        Ok(Self {
            bookkeeping: crate::storage::Bookkeeping::new_owned(
                &domain,
                crate::storage::OwnerKind::Operation,
            )?,
            writer: Some(WriteMode::Raw(writer)),
            finished_summary: None,
            chunk_mode,
            canonical_schemas: CanonicalMap::new(domain.clone()),
            canonical_channels: CanonicalMap::new(domain.clone()),
            all_channel_ids: crate::u16_table::U16Table::new_owned(
                domain.clone(),
                crate::storage::ResourceCategory::Declaration,
                crate::storage::OwnerKind::Operation,
            ),
            all_schema_ids: crate::u16_table::U16Table::new_owned(
                domain.clone(),
                crate::storage::ResourceCategory::Declaration,
                crate::storage::OwnerKind::Operation,
            ),
            next_channel_id: 1,
            next_schema_id: 1,
            chunk_indexes: crate::segmented::SharedSegmentedVec::new_owned(
                domain.clone(),
                crate::storage::OwnerKind::Operation,
            ),
            attachment_count: 0,
            attachment_indexes: crate::segmented::SharedSegmentedVec::new_owned(
                domain.clone(),
                crate::storage::OwnerKind::Operation,
            ),
            metadata_count: 0,
            metadata_indexes: crate::segmented::SharedSegmentedVec::new_owned(
                domain.clone(),
                crate::storage::OwnerKind::Operation,
            ),
            message_bounds: None,
            channel_message_counts: crate::u16_table::SharedU16Table::new_owned(
                domain.clone(),
                crate::storage::ResourceCategory::Index,
                crate::storage::OwnerKind::Operation,
            ),
            options: opts,
            domain,
        })
    }

    /// Adds a schema, returning its ID. If a schema with the same content has been added already,
    /// its ID is returned.
    ///
    /// * `name`: an identifier for the schema.
    /// * `encoding`: Describes the schema format.  The [well-known schema
    ///   encodings](https://mcap.dev/spec/registry#well-known-schema-encodings) are preferred. An
    ///   empty string indicates no schema is available.
    /// * `data`: The serialized schema content. If `encoding` is an empty string, `data` should
    ///   have zero length.
    pub fn add_schema(&mut self, name: &str, encoding: &str, data: &[u8]) -> McapResult<u16> {
        let content = SchemaContent {
            name: name.into(),
            encoding: encoding.into(),
            data: data.into(),
        };
        if let Some(&id) = self
            .canonical_schemas
            .find_by(|stored| content.compare(stored))
        {
            return Ok(id);
        }
        while self.all_schema_ids.contains_key(&self.next_schema_id) {
            if self.next_schema_id == u16::MAX {
                return Err(McapError::TooManySchemas);
            }
            self.next_schema_id += 1;
        }
        let id = self.next_schema_id;
        self.next_schema_id += 1;
        self.write_schema(id, name, encoding, data)?;
        self.canonical_schemas
            .insert_no_overwrite(content.into_owned(&self.domain)?, id)?;
        assert!(self.all_schema_ids.insert_fixed(id, id)?.is_none());
        Ok(id)
    }

    /// Adds a schema with an explicit ID.
    ///
    /// Unlike [`Self::add_schema`], this method does not coalesce away schema records with
    /// duplicate content. This is useful when preserving distinct schema IDs from an input file
    /// is required, even if multiple schemas share identical name/encoding/data.
    ///
    /// Adding an explicit ID does not advance the internal allocator used by
    /// [`Self::add_schema`]. Automatically allocated IDs continue to use the first available
    /// schema ID.
    ///
    /// If a schema with the same ID is already known:
    /// - returns `Ok(id)` if its content matches
    /// - returns [`McapError::ConflictingSchemas`] if its content differs
    ///
    /// Schema ID 0 is not valid and returns [`McapError::InvalidSchemaId`].
    ///
    /// When mixing this API with [`Self::add_schema`], whichever ID is canonical for a given
    /// schema content tuple is returned by `add_schema`. So if a schema content is first
    /// registered via `add_schema_with_id(9000, ...)`, a later `add_schema(...)` with the same
    /// content returns `9000`.
    pub fn add_schema_with_id(
        &mut self,
        id: u16,
        name: &str,
        encoding: &str,
        data: &[u8],
    ) -> McapResult<u16> {
        if id == 0 {
            return Err(McapError::InvalidSchemaId);
        }

        let content = SchemaContent {
            name: Cow::Borrowed(name),
            encoding: Cow::Borrowed(encoding),
            data: Cow::Borrowed(data),
        };

        if let Some(existing_canonical_id) = self.all_schema_ids.get(&id).copied() {
            let Some(current_canonical_id) = self
                .canonical_schemas
                .find_by(|stored| content.compare(stored))
                .copied()
            else {
                return Err(McapError::ConflictingSchemas(name.into()));
            };
            if existing_canonical_id != current_canonical_id {
                return Err(McapError::ConflictingSchemas(name.into()));
            }
            return Ok(id);
        }

        self.write_schema(id, name, encoding, data)?;

        if let Some(canonical_id) = self
            .canonical_schemas
            .find_by(|stored| content.compare(stored))
            .copied()
        {
            self.all_schema_ids.insert_fixed(id, canonical_id)?;
        } else {
            self.canonical_schemas
                .insert_no_overwrite(content.into_owned(&self.domain)?, id)?;
            self.all_schema_ids.insert_fixed(id, id)?;
        }

        Ok(id)
    }

    /// Write a schema record into the MCAP.
    fn write_schema(&mut self, id: u16, name: &str, encoding: &str, data: &[u8]) -> McapResult<()> {
        if data.len() > self.domain.limits().block {
            return Err(McapError::ChunkBufferTooLarge(data.len() as u64));
        }
        let header = records::SchemaHeaderRef { id, name, encoding };
        if self.options.use_chunks {
            self.start_chunk()?.serialize(|sink| write_schema_record(sink, &header, data))
        } else {
            Ok(write_schema_record(self.finish_chunk()?, &header, data)?)
        }
    }

    /// Adds a channel, returning its ID. If a channel with equivalent content was added previously,
    /// its ID is returned.
    ///
    /// Useful with subsequent calls to [`write_to_known_channel()`](Self::write_to_known_channel).
    ///
    /// * `schema_id`: a schema_id returned from [`Self::add_schema`], or 0 if the channel has no
    ///   schema.
    /// * `topic`: The topic name.
    /// * `message_encoding`: Encoding for messages on this channel. The [well-known message
    ///   encodings](https://mcap.dev/spec/registry#well-known-message-encodings) are preferred.
    ///  * `metadata`: Metadata about this channel.
    pub fn add_channel(
        &mut self,
        schema_id: u16,
        topic: &str,
        message_encoding: &str,
        metadata: &BTreeMap<String, String>,
    ) -> McapResult<u16> {
        self.add_channel_borrowed(
            schema_id,
            topic,
            message_encoding,
            metadata.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
    }

    /// Borrows stable, repeatable metadata pairs in strictly increasing key order.
    /// Input is consumed synchronously; invalid ordering is rejected before advancement.
    pub fn add_channel_borrowed<'a, I: Clone + Iterator<Item = (&'a str, &'a str)>>(
        &mut self,
        schema_id: u16,
        topic: &'a str,
        message_encoding: &'a str,
        metadata: I,
    ) -> McapResult<u16> {
        validate_channel_pairs(metadata.clone())?;
        let content = ChannelContent {
            topic: Cow::Borrowed(topic),
            schema_id,
            message_encoding: Cow::Borrowed(message_encoding),
            metadata: metadata.clone(),
        };
        if let Some(&id) = self
            .canonical_channels
            .find_by(|stored| content.compare(stored))
        {
            return Ok(id);
        }
        if schema_id != 0 && !self.all_schema_ids.contains_key(&schema_id) {
            return Err(McapError::UnknownSchema(topic.into(), schema_id));
        }

        while self.all_channel_ids.contains_key(&self.next_channel_id) {
            if self.next_channel_id == u16::MAX {
                return Err(McapError::TooManyChannels);
            }
            self.next_channel_id += 1;
        }
        let id = self.next_channel_id;
        self.next_channel_id += 1;
        self.write_channel(records::ChannelPairsRef {
            id,
            schema_id,
            topic,
            message_encoding,
            metadata,
        })?;
        self.canonical_channels
            .insert_no_overwrite(content.into_owned(&self.domain)?, id)?;
        assert!(self.all_channel_ids.insert_fixed(id, id)?.is_none());
        Ok(id)
    }

    /// Adds a channel with an explicit ID.
    ///
    /// Unlike [`Self::add_channel`], this method does not coalesce away channel records with
    /// duplicate content. This is useful when preserving distinct channel IDs from an input file
    /// is required, even if multiple channels share identical topic/schema/encoding/metadata.
    ///
    /// Adding an explicit ID does not advance the internal allocator used by
    /// [`Self::add_channel`]. Automatically allocated IDs continue to use the first available
    /// channel ID.
    ///
    /// If a channel with the same ID is already known:
    /// - returns `Ok(id)` if its content matches
    /// - returns [`McapError::ConflictingChannels`] if its content differs
    ///
    /// Channel ID 0 is accepted.
    ///
    /// When mixing this API with [`Self::add_channel`], whichever ID is canonical for a given
    /// channel content tuple is returned by `add_channel`. So if a channel content is first
    /// registered via `add_channel_with_id(9000, ...)`, a later `add_channel(...)` with the same
    /// content returns `9000`.
    pub fn add_channel_with_id(
        &mut self,
        id: u16,
        schema_id: u16,
        topic: &str,
        message_encoding: &str,
        metadata: &BTreeMap<String, String>,
    ) -> McapResult<u16> {
        self.add_channel_with_id_borrowed(
            id,
            schema_id,
            topic,
            message_encoding,
            metadata.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
    }

    /// Borrows stable, repeatable metadata pairs in strictly increasing key order.
    /// Input is consumed synchronously; invalid ordering is rejected before advancement.
    pub fn add_channel_with_id_borrowed<'a, I: Clone + Iterator<Item = (&'a str, &'a str)>>(
        &mut self,
        id: u16,
        schema_id: u16,
        topic: &'a str,
        message_encoding: &'a str,
        metadata: I,
    ) -> McapResult<u16> {
        validate_channel_pairs(metadata.clone())?;
        if schema_id != 0 && !self.all_schema_ids.contains_key(&schema_id) {
            return Err(McapError::UnknownSchema(topic.into(), schema_id));
        }

        let content = ChannelContent {
            topic: Cow::Borrowed(topic),
            schema_id,
            message_encoding: Cow::Borrowed(message_encoding),
            metadata: metadata.clone(),
        };

        if let Some(existing_canonical_id) = self.all_channel_ids.get(&id).copied() {
            let Some(current_canonical_id) = self
                .canonical_channels
                .find_by(|stored| content.compare(stored))
                .copied()
            else {
                return Err(McapError::ConflictingChannels(topic.into()));
            };
            if existing_canonical_id != current_canonical_id {
                return Err(McapError::ConflictingChannels(topic.into()));
            }
            return Ok(id);
        }

        self.write_channel(records::ChannelPairsRef {
            id,
            schema_id,
            topic,
            message_encoding,
            metadata,
        })?;

        if let Some(canonical_id) = self
            .canonical_channels
            .find_by(|stored| content.compare(stored))
            .copied()
        {
            self.all_channel_ids.insert_fixed(id, canonical_id)?;
        } else {
            self.canonical_channels
                .insert_no_overwrite(content.into_owned(&self.domain)?, id)?;
            self.all_channel_ids.insert_fixed(id, id)?;
        }

        Ok(id)
    }

    /// Write a channel record into the MCAP.
    fn write_channel<'a, I: Clone + Iterator<Item = (&'a str, &'a str)>>(
        &mut self,
        channel: records::ChannelPairsRef<'a, I>,
    ) -> McapResult<()> {
        if self.options.use_chunks {
            self.start_chunk()?.serialize(|sink| write_record_body(sink, op::CHANNEL, &channel))
        } else {
            Ok(write_record_body(
                self.finish_chunk()?,
                op::CHANNEL,
                &channel,
            )?)
        }
    }

    /// Write the given message (and its provided channel, if not already added).
    /// The provided channel ID and schema ID will be used as IDs in the resulting MCAP.
    pub fn write(&mut self, message: &Message) -> McapResult<()> {
        self.write_borrowed(
            message.channel.id,
            &message.channel.topic,
            &message.channel.message_encoding,
            message
                .channel
                .metadata
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
            message
                .channel
                .schema
                .as_ref()
                .map(|s| (s.id, s.name.as_str(), s.encoding.as_str(), s.data.as_ref())),
            &MessageHeader {
                channel_id: message.channel.id,
                sequence: message.sequence,
                log_time: message.log_time,
                publish_time: message.publish_time,
            },
            &message.data,
        )
    }

    /// Writes a full message using borrowed declaration fields. Metadata pairs
    /// must be stable, repeatable and ordered by unique key. Input is consumed
    /// before return; retained declaration content is copied into charged storage.
    pub fn write_borrowed<'a, I: Clone + Iterator<Item = (&'a str, &'a str)>>(
        &mut self,
        channel_id: u16,
        topic: &'a str,
        message_encoding: &'a str,
        metadata: I,
        schema: Option<(u16, &'a str, &'a str, &'a [u8])>,
        header: &MessageHeader,
        data: &[u8],
    ) -> McapResult<()> {
        if header.channel_id != channel_id {
            return Err(io::Error::from(io::ErrorKind::InvalidInput).into());
        }
        validate_channel_pairs(metadata.clone())?;
        if let Some((schema_id, name, encoding, schema_data)) = schema {
            if schema_data.len() > self.domain.limits().block {
                return Err(McapError::ChunkBufferTooLarge(schema_data.len() as u64));
            }
            let content = SchemaContent {
                name: Cow::Borrowed(name),
                encoding: Cow::Borrowed(encoding),
                data: Cow::Borrowed(schema_data),
            };
            let canonical_schema_id = self
                .canonical_schemas
                .find_by(|stored| content.compare(stored));
            match self.all_schema_ids.get(&schema_id).copied() {
                Some(other) => {
                    // ensure that this message schema does not conflict with the existing one's content
                    let canonical_schema_id = canonical_schema_id
                        .expect("all values in all_schema_ids should be canonical schema IDs");
                    if other != *canonical_schema_id {
                        return Err(McapError::ConflictingSchemas(name.to_owned()));
                    }
                }
                None => {
                    // no previous schema has been written with this ID, but one may have with the same content.
                    // this is OK.
                    if let Some(canonical_schema_id) = canonical_schema_id {
                        self.all_schema_ids
                            .insert_fixed(schema_id, *canonical_schema_id)?;
                    } else {
                        self.canonical_schemas.insert_no_overwrite(
                            content.into_owned(&self.domain)?,
                            schema_id,
                        )?;
                        self.all_schema_ids.insert_fixed(schema_id, schema_id)?;
                    }
                    self.write_schema(schema_id, name, encoding, schema_data)?;
                }
            }
        }
        let schema_id = schema.map_or(0, |s| s.0);
        let channel_content = ChannelContent {
            topic: Cow::Borrowed(topic),
            schema_id,
            message_encoding: Cow::Borrowed(message_encoding),
            metadata: metadata.clone(),
        };
        let canonical_channel_id = self
            .canonical_channels
            .find_by(|stored| channel_content.compare(stored));
        match self.all_channel_ids.get(&channel_id).copied() {
            Some(other) => {
                let canonical_channel_id = canonical_channel_id
                    .expect("values in all_channel_ids should be valid canonical channel IDs");
                if *canonical_channel_id != other {
                    return Err(McapError::ConflictingChannels(topic.to_owned()));
                }
            }
            None => {
                // no previous channel has been written with this ID, but one may have with the same content.
                // this is OK.
                if let Some(canonical_channel_id) = canonical_channel_id {
                    self.all_channel_ids
                        .insert_fixed(channel_id, *canonical_channel_id)?;
                } else {
                    self.canonical_channels.insert_no_overwrite(
                        channel_content.into_owned(&self.domain)?,
                        channel_id,
                    )?;
                    self.all_channel_ids.insert_fixed(channel_id, channel_id)?;
                }
                self.write_channel(records::ChannelPairsRef {
                    id: channel_id,
                    schema_id: schema_id,
                    topic: topic,
                    message_encoding: message_encoding,
                    metadata: metadata.clone(),
                })?;
            }
        }
        self.write_to_known_channel(header, data)
    }

    /// Write a message to an added channel, given its ID.
    ///
    /// This skips hash lookups of the channel and schema if you already added them.
    /// Tests channel registration without mutating writer state.
    pub fn contains_channel(&self, id: u16) -> bool {
        self.all_channel_ids.contains_key(&id)
    }

    pub fn write_to_known_channel(
        &mut self,
        header: &MessageHeader,
        data: &[u8],
    ) -> McapResult<()> {
        if !self.all_channel_ids.contains_key(&header.channel_id) {
            return Err(McapError::UnknownChannel(
                header.sequence,
                header.channel_id,
            ));
        }

        self.message_bounds = Some(match self.message_bounds {
            None => (header.log_time, header.log_time),
            Some((start, end)) => (start.min(header.log_time), end.max(header.log_time)),
        });
        let count = self
            .channel_message_counts
            .get(&header.channel_id)
            .copied()
            .unwrap_or(0);
        self.channel_message_counts
            .insert_fixed(header.channel_id, count + 1)?;

        // if the current chunk is larger than our target chunk size, finish it
        // and start a new one.
        if let (Some(WriteMode::Chunk(cw)), Some(target)) = (&self.writer, self.options.chunk_size)
        {
            let current_chunk_size = cw.compressor.position();
            if current_chunk_size > target {
                self.finish_chunk()?;
            }
        }

        if self.options.use_chunks {
            self.start_chunk()?.write_message(header, data)?;
        } else {
            write_record(
                self.finish_chunk()?,
                &Record::Message {
                    header: *header,
                    data: Cow::Borrowed(data),
                },
            )?;
        }
        Ok(())
    }

    /// Write a private record using the provided options.
    ///
    /// Private records must have an opcode >= 0x80.
    pub fn write_private_record(
        &mut self,
        opcode: u8,
        data: &[u8],
        options: EnumSet<PrivateRecordOptions>,
    ) -> McapResult<()> {
        if opcode < 0x80 {
            return Err(McapError::PrivateRecordOpcodeIsReserved { opcode });
        }

        let record = Record::Unknown {
            opcode,
            data: Cow::Borrowed(data),
        };

        if self.options.use_chunks && options.contains(PrivateRecordOptions::IncludeInChunks) {
            self.start_chunk()?.write_record(&record)?;
        } else {
            write_record(self.finish_chunk()?, &record)?;
        }

        Ok(())
    }

    /// Start writing an attachment.
    ///
    /// This is a low level API. For small attachments, use [`Self::attach`].
    ///
    /// To start writing an attachment call this method with the [`AttachmentHeader`] as well as
    /// the length of the attachment in bytes. It is important this length is exact otherwise the
    /// writer will be left in an error state.
    ///
    /// This call should be followed by one or more calls to [`Self::put_attachment_bytes`].
    ///
    /// Once all attachment bytes have been written the attachment must be completed with a call to
    /// [`Self::finish_attachment`]. Failing to finish the attachment will leave the write in an
    /// error state.
    ///
    /// # Example
    /// ```rust
    /// # use mcap::write::Writer;
    /// # use mcap::records::AttachmentHeader;
    /// #
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut output = vec![];
    /// # let mut writer = Writer::new(std::io::Cursor::new(&mut output))?;
    /// let attachment_length = 6;
    ///
    /// // Start the attachment
    /// writer.start_attachment(attachment_length, AttachmentHeader {
    ///     log_time: 100,
    ///     create_time: 200,
    ///     name: "my-attachment".into(),
    ///     media_type: "application/octet-stream".into()
    /// })?;
    ///
    /// // Write all the bytes for the attachment. The amount of bytes written must
    /// // match the length specified when the attachment was started.
    /// writer.put_attachment_bytes(&[ 1, 2, 3, 4 ])?;
    /// writer.put_attachment_bytes(&[ 5, 6 ])?;
    ///
    /// // Finish writing the attachment.
    /// writer.finish_attachment()?;
    /// #
    /// # Ok(())
    /// # }
    /// # run().expect("should succeed");
    /// ```
    pub fn start_attachment(
        &mut self,
        attachment_length: u64,
        header: AttachmentHeader,
    ) -> McapResult<()> {
        self.start_attachment_borrowed(
            attachment_length,
            header.log_time,
            header.create_time,
            &header.name,
            &header.media_type,
        )
    }

    /// Consumes borrowed header fields synchronously. Only retained indexes copy
    /// strings, directly into their final, budgeted storage before header output.
    pub fn start_attachment_borrowed(
        &mut self,
        attachment_length: u64,
        log_time: u64,
        create_time: u64,
        name: &str,
        media_type: &str,
    ) -> McapResult<()> {
        self.finish_chunk()?;
        let WriteMode::Raw(w) = self.writer.take().expect(Self::WRITER_IS_NONE) else {
            unreachable!("finish_chunk establishes raw write mode");
        };
        let header = records::AttachmentHeaderRef {
            log_time,
            create_time,
            name,
            media_type,
        };
        match AttachmentWriter::new(
            w,
            attachment_length,
            header,
            self.options.calculate_attachment_crcs,
            self.options.emit_attachment_indexes,
            &self.domain,
        ) {
            Ok(writer) => self.writer = Some(WriteMode::Attachment(writer)),
            Err((writer, error)) => {
                self.writer = Some(WriteMode::Failed(writer.finalize().0));
                return Err(error);
            }
        }
        Ok(())
    }

    /// Write bytes to the current attachment.
    ///
    /// This is a low level API. For small attachments, use [`Self::attach`].
    ///
    /// Before calling this method call [`Self::start_attachment`].
    pub fn put_attachment_bytes(&mut self, bytes: &[u8]) -> McapResult<()> {
        let Some(WriteMode::Attachment(writer)) = &mut self.writer else {
            return Err(McapError::AttachmentNotInProgress);
        };

        writer.put_bytes(bytes)?;

        Ok(())
    }

    /// Finish the current attachment.
    ///
    /// This is a low level API. For small attachments, use [`Self::attach`].
    ///
    /// Before calling this method call [`Self::start_attachment`] and write bytes to the
    /// attachment using [`Self::put_attachment_bytes`].
    pub fn finish_attachment(&mut self) -> McapResult<()> {
        let Some(WriteMode::Attachment(..)) = &mut self.writer else {
            return Err(McapError::AttachmentNotInProgress);
        };

        let Some(WriteMode::Attachment(writer)) = self.writer.take() else {
            panic!("WriteMode is guaranteed to be attachment by this point");
        };

        let (writer, attachment_index) = match writer.finish() {
            Ok(result) => result,
            Err((writer, error)) => {
                self.writer = Some(WriteMode::Failed(writer.finalize().0));
                return Err(error);
            }
        };
        self.attachment_count += 1;

        if let Some(attachment_index) = attachment_index {
            if let Err(error) = self.attachment_indexes.push_fixed(attachment_index) {
                self.writer = Some(WriteMode::Failed(writer.finalize().0));
                return Err(error.into());
            }
        }

        self.writer = Some(WriteMode::Raw(writer));

        Ok(())
    }

    /// Write an attachment to the MCAP file. This finishes any current chunk before writing the
    /// attachment.
    pub fn attach(&mut self, attachment: &Attachment) -> McapResult<()> {
        self.attach_borrowed(
            attachment.log_time,
            attachment.create_time,
            &attachment.name,
            &attachment.media_type,
            &attachment.data,
        )
    }

    /// Writes borrowed attachment fields and payload without temporary owned headers.
    pub fn attach_borrowed(
        &mut self,
        log_time: u64,
        create_time: u64,
        name: &str,
        media_type: &str,
        data: &[u8],
    ) -> McapResult<()> {
        self.start_attachment_borrowed(data.len() as u64, log_time, create_time, name, media_type)?;
        self.put_attachment_bytes(data)?;
        self.finish_attachment()
    }

    /// Write a [Metadata](https://mcap.dev/spec#metadata-op0x0c) record to the MCAP file. This
    /// finishes any current chunk before writing the metadata.
    pub fn write_metadata(&mut self, metadata: &Metadata) -> McapResult<()> {
        self.write_metadata_borrowed(
            &metadata.name,
            metadata
                .metadata
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        )
    }

    /// Writes borrowed metadata without cloning the name or map for serialization.
    /// The iterator must yield unique keys in sorted order and its clones must
    /// produce identical pairs. Input is consumed synchronously before returning.
    pub fn write_metadata_borrowed<'a, I>(&mut self, name: &'a str, pairs: I) -> McapResult<()>
    where
        I: Clone + Iterator<Item = (&'a str, &'a str)>,
    {
        let w = self.finish_chunk()?;
        let offset = w.stream_position()?;

        write_record_body(w, op::METADATA, &records::MetadataRef { name, pairs })?;

        let length = w.stream_position()? - offset;

        self.metadata_count += 1;
        if self.options.emit_metadata_indexes {
            let result = crate::shared_metadata_index::SharedMetadataIndex::new(
                offset,
                length,
                name,
                &self.domain,
                crate::storage::OwnerKind::Operation,
            )
            .and_then(|index| self.metadata_indexes.push_fixed(index));
            if let Err(error) = result {
                let Some(WriteMode::Raw(writer)) = self.writer.take() else {
                    unreachable!()
                };
                self.writer = Some(WriteMode::Failed(writer.finalize().0));
                return Err(error.into());
            }
        }

        Ok(())
    }

    /// Finishes the current chunk, if we have one, and flushes the underlying
    /// [writer](Write).
    ///
    /// We finish the chunk to guarantee that the file can be streamed by future
    /// readers at least up to this point.
    /// (The alternative is to just flush the writer mid-chunk.
    /// But if we did that, and then writing was suddenly interrupted afterwards,
    /// readers would have to try to recover a half-written chunk,
    /// probably with an unfinished compression stream.)
    ///
    /// Note that lossless compression schemes like LZ4 and Zstd improve
    /// as they go, so larger chunks will tend to have better compression.
    /// (Of course, this depends heavily on the entropy of what's being compressed!
    /// A stream of zeroes will compress great at any chunk size, and a stream
    /// of random data will compress terribly at any chunk size.)
    pub fn flush(&mut self) -> McapResult<()> {
        self.finish_chunk()?.flush()?;
        Ok(())
    }

    const WRITER_IS_NONE: &'static str = "unreachable: self.writer should never be None";

    fn assert_not_finished(&self) {
        assert!(
            self.finished_summary.is_none(),
            "{}",
            "Trying to write a record on a finished MCAP"
        );
    }

    /// Starts a new chunk if we haven't done so already.
    fn start_chunk(&mut self) -> McapResult<&mut ChunkWriter<W>> {
        self.assert_not_finished();

        // It is not possible to start writing a chunk if we're still writing an attachment. Return
        // an error instead.
        if let Some(WriteMode::Attachment(..)) = self.writer {
            return Err(McapError::AttachmentNotInProgress);
        }

        assert!(
            self.options.use_chunks,
            "Trying to write to a chunk when chunking is disabled"
        );

        // Rust forbids moving values out of a &mut reference. We made self.writer an Option so we
        // can work around this by using take() to temporarily replace it with None while we
        // construct the ChunkWriter.
        self.writer = Some(match self.writer.take().expect(Self::WRITER_IS_NONE) {
            WriteMode::Raw(w) => {
                // It's chunkin time.
                WriteMode::Chunk(ChunkWriter::new(
                    w,
                    self.options.compression,
                    std::mem::take(&mut self.chunk_mode),
                    self.options.emit_message_indexes,
                    self.domain.clone(),
                    self.options.calculate_chunk_crcs,
                    #[cfg(any(feature = "zstd", feature = "lz4"))]
                    self.options.compression_level,
                    #[cfg(feature = "zstd")]
                    self.options.compression_threads.expect("thread count resolved at creation"),
                )?)
            }
            chunk => chunk,
        });
        if let Some(WriteMode::Failed(_)) = &self.writer {
            return Err(McapError::AttemptedWriteAfterFailure);
        }

        let Some(WriteMode::Chunk(c)) = &mut self.writer else {
            unreachable!("we're not in an attachment and write mode was set to chunk above")
        };

        Ok(c)
    }

    /// Finish the current chunk, if we have one.
    fn finish_chunk(&mut self) -> McapResult<&mut CountingCrcWriter<W>> {
        self.assert_not_finished();
        // If we're currently writing an attachment then we're not writing a chunk. Return an
        // error instead.
        if let Some(WriteMode::Attachment(..)) = self.writer {
            return Err(McapError::AttachmentNotInProgress);
        }

        // See start_chunk() for why we use take() here.
        match self.writer.take().expect(Self::WRITER_IS_NONE) {
            WriteMode::Chunk(c) => match c.finish() {
                Ok((w, mode, index)) => {
                    if let Err(error) = self.chunk_indexes.push_fixed(index) {
                        self.writer = Some(WriteMode::Failed(w.finalize().0));
                        return Err(error.into());
                    }
                    self.chunk_mode = mode;
                    self.writer = Some(WriteMode::Raw(w))
                }
                Err((w, err)) => {
                    self.writer = Some(WriteMode::Failed(w));
                    return Err(err);
                }
            },
            WriteMode::Failed(w) => {
                self.writer = Some(WriteMode::Failed(w));
                return Err(McapError::AttemptedWriteAfterFailure);
            }
            mode => self.writer = Some(mode),
        };

        let Some(WriteMode::Raw(w)) = &mut self.writer else {
            unreachable!("we're not in an attachment and write mode raw was set above")
        };

        Ok(w)
    }

    /// Finishes any current chunk and writes out the summary section of the file.
    ///
    /// Returns a [`Summary`] of data written to the file.  Subsequent calls to other methods will
    /// panic.
    pub fn finish(&mut self) -> McapResult<Summary> {
        if let Some(summary) = &self.finished_summary {
            // We already called finish().
            // Maybe we're dropping after the user called it?
            return Ok(summary.clone());
        }
        // Finish any chunk we were working on and update stats, indexes, etc.
        self.finish_chunk()?;

        let summary = match self.take_summary() {
            Ok(summary) => summary,
            Err(error) => {
                // Summary creation consumes statistics; refusal cannot be retried.
                let Some(WriteMode::Raw(writer)) = self.writer.take() else {
                    unreachable!()
                };
                self.writer = Some(WriteMode::Failed(writer.finalize().0));
                return Err(error);
            }
        };
        self.finished_summary = Some(summary.clone());

        // Grab the writer - self.writer becoming None makes subsequent writes fail.
        let writer = match &mut self.writer {
            // We called finish_chunk() above, so we're back to raw writes for
            // the summary section.
            Some(WriteMode::Raw(w)) => w,
            _ => unreachable!(),
        };
        let data_section_crc = writer.current_checksum();
        let writer = writer.get_mut();
        // We're done with the data section!
        write_record(
            writer,
            &Record::DataEnd(records::DataEnd { data_section_crc }),
        )?;
        write_summary_and_footer_magic(writer, &summary, &self.options)?;
        Ok(summary)
    }

    /// moves writer bookkeeping fields into a summary struct, which can be returned on finish.
    fn take_summary(&mut self) -> McapResult<Summary> {
        // Grab stats before we munge all the self fields below.
        let message_bounds = self.message_bounds.unwrap_or((0, 0));
        let channel_message_counts = std::mem::replace(
            &mut self.channel_message_counts,
            crate::u16_table::SharedU16Table::new_owned(
                self.domain.clone(),
                crate::storage::ResourceCategory::Index,
                crate::storage::OwnerKind::Operation,
            ),
        );
        let stats = crate::shared_statistics::SharedStatistics {
            fields: crate::shared_statistics::StatisticsFields {
                message_count: channel_message_counts.values().sum(),
                schema_count: self.all_schema_ids.len() as u16,
                channel_count: self.all_channel_ids.len() as u32,
                attachment_count: self.attachment_count,
                metadata_count: self.metadata_count,
                chunk_count: self.chunk_indexes.len() as u32,
                message_start_time: message_bounds.0,
                message_end_time: message_bounds.1,
            },
            channel_message_counts,
        };
        let mut schemas = crate::u16_table::SharedU16Table::new_owned(
            self.domain.clone(),
            crate::storage::ResourceCategory::Declaration,
            crate::storage::OwnerKind::Operation,
        );
        let mut channels = crate::u16_table::SharedU16Table::new_owned(
            self.domain.clone(),
            crate::storage::ResourceCategory::Declaration,
            crate::storage::OwnerKind::Operation,
        );
        for (schema_id, canonical_id) in self.all_schema_ids.iter() {
            let schema_content = self
                .canonical_schemas
                .get_by_right(canonical_id)
                .expect("schema content must be present for canonical id");
            schemas.insert_fixed(
                schema_id,
                crate::shared_declarations::SharedSchema::new(
                    schema_id, schema_content.name.text(), schema_content.encoding.text(),
                    schema_content.data.bytes(), &self.domain, crate::storage::OwnerKind::Operation,
                )?,
            )?;
        }
        for (channel_id, canonical_id) in self.all_channel_ids.iter() {
            let channel_content = self
                .canonical_channels
                .get_by_right(canonical_id)
                .expect("channel content must be present for canonical id");
            channels.insert_fixed(
                channel_id,
                crate::shared_declarations::SharedChannel::new(
                    channel_id, channel_content.topic.text(), channel_content.message_encoding.text(),
                    schemas.get(&channel_content.schema_id).cloned(), channel_content.metadata(),
                    &self.domain, crate::storage::OwnerKind::Operation,
                )?,
            )?;
        }
        Ok(Summary {
            bookkeeping: self.bookkeeping.clone(),
            stats: Some(stats),
            channels,
            schemas,
            chunk_indexes: std::mem::replace(
                &mut self.chunk_indexes,
                crate::segmented::SharedSegmentedVec::new_owned(
                    self.domain.clone(),
                    crate::storage::OwnerKind::Operation,
                ),
            ),
            attachment_indexes: std::mem::replace(
                &mut self.attachment_indexes,
                crate::segmented::SharedSegmentedVec::new_owned(
                    self.domain.clone(),
                    crate::storage::OwnerKind::Operation,
                ),
            ),
            metadata_indexes: std::mem::replace(
                &mut self.metadata_indexes,
                crate::segmented::SharedSegmentedVec::new_owned(
                    self.domain.clone(),
                    crate::storage::OwnerKind::Operation,
                ),
            ),
        })
    }

    /// Consumes this writer, returning the underlying stream. Unless [`Self::finish()`] was called
    /// first, the underlying stream __will not contain a complete MCAP.__
    ///
    /// Use this if you wish to handle any errors returned when the underlying stream is closed. In
    /// particular, if using [`std::fs::File`], you may wish to call [`std::fs::File::sync_all()`]
    /// to ensure all data was sent to the filesystem.
    pub fn into_inner(mut self) -> W {
        // Peel away all the layers of the writer to get the underlying stream.
        match self.writer.take().expect(Self::WRITER_IS_NONE) {
            WriteMode::Raw(w) => w.finalize().0,
            WriteMode::Attachment(w) => w.writer.finalize().0.finalize().0,
            WriteMode::Chunk(w) => w.compressor.finalize().0.into_inner().finalize().0.inner,
            WriteMode::Failed(w) => w,
        }
    }
}

impl<W: Write + Seek> Drop for Writer<W> {
    fn drop(&mut self) {
        // Extraction must not build a summary just to suppress implicit finish.
        // The stream is absent after into_inner, including during unwinding.
        if self.writer.is_some() {
            let _ = self.finish();
        }
    }
}

/// Write out summary section, footer and end magic to the file.
fn write_summary_and_footer_magic<W: Write + Seek>(
    writer: &mut W,
    summary: &Summary,
    options: &WriteOptions,
) -> McapResult<()> {
    let summary_start = writer.stream_position()?;
    let summary_offset_start;
    // Let's get a CRC of the summary section.
    let mut ccw;

    // At most one offset for each of the six summary record groups.
    let mut offsets: [Option<records::SummaryOffset>; 6] = std::array::from_fn(|_| None);
    let mut offset_count = 0;
    let mut add_offset = |offset| {
        offsets[offset_count] = Some(offset);
        offset_count += 1;
    };

    let mut summary_end = summary_start;
    ccw = CountingCrcWriter::new(writer, options.calculate_summary_section_crc);

    fn posit<W: Write + Seek>(ccw: &mut CountingCrcWriter<W>) -> io::Result<u64> {
        ccw.get_mut().stream_position()
    }

    // Write all schemas.
    if options.repeat_schemas && !summary.schemas.is_empty() {
        let schemas_start: u64 = summary_start;
        for (id, schema) in summary.schemas.iter() {
            let header = records::SchemaHeaderRef {
                id,
                name: &schema.name,
                encoding: &schema.encoding,
            };
            write_schema_record(&mut ccw, &header, schema.data.as_ref())?;
        }
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::SCHEMA,
            group_start: schemas_start,
            group_length: summary_end - schemas_start,
        });
    }

    // Write all channels.
    if options.repeat_channels && !summary.channels.is_empty() {
        let channels_start = summary_end;
        for (id, channel) in summary.channels.iter() {
            let channel = records::ChannelPairsRef {
                id,
                schema_id: channel.schema.as_ref().map_or(0, |schema| schema.id),
                topic: &channel.topic,
                message_encoding: &channel.message_encoding,
                metadata: channel.metadata.iter().map(|(k,v)|(k.as_str(),v.as_str())),
            };
            write_record_body(&mut ccw, op::CHANNEL, &channel)?;
        }
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::CHANNEL,
            group_start: channels_start,
            group_length: summary_end - channels_start,
        });
    }

    if options.emit_statistics {
        let statistics_start = summary_end;
        write_record_body(
            &mut ccw,
            op::STATISTICS,
            summary
                .stats
                .as_ref()
                .expect("summarize always emits Some(stats)"),
        )?;
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::STATISTICS,
            group_start: statistics_start,
            group_length: summary_end - statistics_start,
        });
    }

    if options.emit_chunk_indexes && !summary.chunk_indexes.is_empty() {
        // Write all chunk indexes.
        let chunk_indexes_start = summary_end;
        for index in &summary.chunk_indexes {
            write_record_body(&mut ccw, op::CHUNK_INDEX, index)?;
        }
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::CHUNK_INDEX,
            group_start: chunk_indexes_start,
            group_length: summary_end - chunk_indexes_start,
        });
    }

    // ...and attachment indexes
    if options.emit_attachment_indexes && !summary.attachment_indexes.is_empty() {
        let attachment_indexes_start = summary_end;
        for index in &summary.attachment_indexes {
            write_record_body(&mut ccw, op::ATTACHMENT_INDEX, index)?;
        }
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::ATTACHMENT_INDEX,
            group_start: attachment_indexes_start,
            group_length: summary_end - attachment_indexes_start,
        });
    }

    // ...and metadata indexes
    if options.emit_metadata_indexes && !summary.metadata_indexes.is_empty() {
        let metadata_indexes_start = summary_end;
        for index in &summary.metadata_indexes {
            write_record_body(&mut ccw, op::METADATA_INDEX, index)?;
        }
        summary_end = posit(&mut ccw)?;
        add_offset(records::SummaryOffset {
            group_opcode: op::METADATA_INDEX,
            group_start: metadata_indexes_start,
            group_length: summary_end - metadata_indexes_start,
        });
    }

    // Write the summary offsets we've been accumulating
    if options.emit_summary_offsets {
        summary_offset_start = summary_end;
        for offset in offsets.into_iter().flatten() {
            write_record(&mut ccw, &Record::SummaryOffset(offset))?;
        }
    } else {
        summary_offset_start = 0;
    }

    let summary_start = if summary_end > summary_start {
        summary_start
    } else {
        0 // We didn't write anything to the summary section.
    };

    // The CRC in the footer _includes_ part of the footer.
    op_and_len(
        &mut ccw,
        op::FOOTER,
        8   // summary start
            + 8   // summary offset start
            + 4, // summary CRC
    )?;
    ccw.write_u64::<LE>(summary_start)?;
    ccw.write_u64::<LE>(summary_offset_start)?;
    let (writer, summary_hasher) = ccw.finalize();
    let summary_crc = summary_hasher.map(|hasher| hasher.finalize()).unwrap_or(0);

    writer.write_u32::<LE>(summary_crc)?;

    writer.write_all(MAGIC)?;
    writer.flush()?;
    Ok(())
}

enum Compressor<W: McapWrite> {
    Null(W),
    // zstd's Encoder wrapper doesn't let us get the inner writer without calling finish(), so use
    // zio::Writer directly instead.
    #[cfg(feature = "zstd")]
    Zstd(crate::codec_writer::zstd_encoder::Encoder<W>),
    #[cfg(feature = "lz4")]
    Lz4(crate::codec_writer::lz4_encoder::Encoder<W>),
}

impl<W: McapWrite> Compressor<W> {
    fn finish(self) -> (W, McapResult<()>) {
        match self {
            Compressor::Null(w) => (w, Ok(())),
            #[cfg(feature = "zstd")]
            Compressor::Zstd(w) => w.finish(),
            #[cfg(feature = "lz4")]
            Compressor::Lz4(w) => w.finish(),
        }
    }

    fn into_inner(self) -> W {
        match self {
            Compressor::Null(w) => w,
            #[cfg(feature = "zstd")]
            Compressor::Zstd(w) => w.into_inner(),
            #[cfg(feature = "lz4")]
            Compressor::Lz4(w) => w.into_inner(),
        }
    }
}

impl<W: McapWrite> Compressor<W> {
    fn write(&mut self, buf: &[u8]) -> McapResult<usize> {
        match self {
            Compressor::Null(w) => w.write_mcap(buf),
            #[cfg(feature = "zstd")]
            Compressor::Zstd(w) => w.write(buf),
            #[cfg(feature = "lz4")]
            Compressor::Lz4(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> McapResult<()> {
        match self {
            Compressor::Null(w) => w.flush_mcap(),
            #[cfg(feature = "zstd")]
            Compressor::Zstd(w) => w.flush(),
            #[cfg(feature = "lz4")]
            Compressor::Lz4(w) => w.flush(),
        }
    }
}

impl<W: McapWrite> McapWrite for Compressor<W> {
    fn write_mcap(&mut self, bytes: &[u8]) -> McapResult<usize> { self.write(bytes) }
    fn flush_mcap(&mut self) -> McapResult<()> { self.flush() }
}
fn serialize_output<W: McapWrite>(writer: &mut W,
    write: impl FnOnce(&mut RecordSink<'_, W>) -> io::Result<()>) -> McapResult<()> {
    let mut sink = RecordSink {compressor: writer, failure: None};
    let result = write(&mut sink);
    if let Some(error) = sink.failure { return Err(error); }
    Ok(result?)
}

// Keep I/O failures out of binrw's diagnostic allocation path. The serializer
// completes against this local facade after the first failure, but no further
// codec or output operation occurs. The record boundary MUST inspect failure.
// Counting/CRC only track writes accepted by the actual compressor.
struct RecordSink<'a, W: McapWrite> {
    compressor: &'a mut W,
    failure: Option<McapError>,
}
impl<W: McapWrite> Write for RecordSink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_none() {
            loop {
                match self.compressor.write_mcap(bytes) {
                    Ok(count) if count != 0 || bytes.is_empty() => return Ok(count),
                    Ok(_) => self.failure = Some(io::Error::from(io::ErrorKind::WriteZero).into()),
                    Err(McapError::Io(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => self.failure = Some(error),
                }
                break;
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.failure.is_none() {
            if let Err(error) = self.compressor.flush_mcap() { self.failure = Some(error); }
        }
        Ok(())
    }
}

struct ChunkWriter<W: Write> {
    chunk_offset: u64,
    header_start: u64,
    data_start: u64,

    /// Message start and end time, or None if there are no messages yet.
    message_bounds: Option<(u64, u64)>,
    compression_name: &'static str,
    compressor: CountingCrcWriter<Compressor<CountingCrcWriter<ChunkSink<W>>>>,
    indexes: crate::u16_table::U16Table<
        crate::segmented::BudgetedSegmentedVec<records::MessageIndexEntry>,
    >,
    index_domain: crate::storage::BudgetRef,

    // Hasher from data before the chunk.
    pre_chunk_crc: Option<crc32fast::Hasher>,

    emit_message_indexes: bool,
}

impl<W: Write + Seek> ChunkWriter<W> {
    fn new(
        mut writer: CountingCrcWriter<W>,
        compression: Option<Compression>,
        mode: ChunkMode,
        emit_message_indexes: bool,
        budget: crate::storage::BudgetRef,
        calculate_chunk_crcs: bool,
        #[cfg(any(feature = "zstd", feature = "lz4"))] compression_level: u32,
        #[cfg(feature = "zstd")] compression_threads: u32,
    ) -> McapResult<Self> {
        // Relative to start of original stream.
        let chunk_offset = writer.stream_position()?;

        let (writer, pre_chunk_crc) = writer.finalize();
        let mut sink = ChunkSink::new(writer, mode);

        // Relative to start of chunk sink stream.
        let header_start = sink.stream_position()?;


        let compression_name = match compression {
            #[cfg(feature = "zstd")]
            Some(Compression::Zstd) => "zstd",
            #[cfg(feature = "lz4")]
            Some(Compression::Lz4) => "lz4",
            #[cfg(not(any(feature = "zstd", feature = "lz4")))]
            Some(_) => unreachable!("`Compression` is an empty enum that cannot be instantiated"),
            None => "",
        };

        // Write a dummy header that we'll overwrite with the actual values later.
        // We just need its size (which only varies based on compression name).
        let header = records::ChunkHeaderRef {
            message_start_time: 0,
            message_end_time: 0,
            uncompressed_size: !0,
            uncompressed_crc: !0,
            compression: compression_name,
            compressed_size: !0,
        };
        serialize_output(&mut sink, |output| {
            op_and_len(output, op::CHUNK, !0)?;
            NoSeek::new(output).write_le(&header).map_err(io::Error::other)
        })?;
        let data_start = sink.stream_position()?;
        let sink = CountingCrcWriter::new(sink, calculate_chunk_crcs);

        let compressor = match compression {
            #[cfg(feature = "zstd")]
            Some(Compression::Zstd) => {
                Compressor::Zstd(crate::codec_writer::zstd_encoder::Encoder::new(
                    sink,
                    compression_level as i32,
                    compression_threads,
                    budget.clone(),
                )?)
            }
            #[cfg(feature = "lz4")]
            Some(Compression::Lz4) => {
                Compressor::Lz4(crate::codec_writer::lz4_encoder::Encoder::new(
                    sink,
                    compression_level,
                    budget.clone(),
                )?)
            }
            #[cfg(not(any(feature = "zstd", feature = "lz4")))]
            Some(_) => unreachable!("`Compression` is an empty enum that cannot be instantiated"),
            None => Compressor::Null(sink),
        };
        let compressor = CountingCrcWriter::new(compressor, calculate_chunk_crcs);
        Ok(Self {
            chunk_offset,
            data_start,
            header_start,
            compressor,
            compression_name,
            message_bounds: None,
            indexes: crate::u16_table::U16Table::new(
                budget.clone(),
                crate::storage::ResourceCategory::Index,
            ),
            index_domain: budget,
            pre_chunk_crc,
            emit_message_indexes,
        })
    }

    fn serialize(&mut self, write: impl FnOnce(&mut RecordSink<'_, CountingCrcWriter<Compressor<CountingCrcWriter<ChunkSink<W>>>>>) -> io::Result<()>)
        -> McapResult<()> {
        serialize_output(&mut self.compressor, write)
    }
    fn write_record(&mut self, record: &Record) -> McapResult<()> {
        self.serialize(|sink| write_record(sink, record))
    }

    fn write_message(&mut self, header: &MessageHeader, data: &[u8]) -> McapResult<()> {
        // Update min/max time for the chunk
        self.message_bounds = Some(match self.message_bounds {
            None => (header.log_time, header.log_time),
            Some((start, end)) => (start.min(header.log_time), end.max(header.log_time)),
        });

        if self.emit_message_indexes {
            // Add an index for this message
            if !self.indexes.contains_key(&header.channel_id) {
                let entries = crate::segmented::BudgetedSegmentedVec::new(
                    self.index_domain.clone(),
                    crate::storage::ResourceCategory::Index,
                );
                self.indexes.insert_fixed(header.channel_id, entries)?;
            }
            let entries = self.indexes.get_mut(&header.channel_id).unwrap();
            entries.push_fixed(records::MessageIndexEntry {
                log_time: header.log_time,
                offset: self.compressor.position(),
            })?;
        }

        self.write_record(&Record::Message {
            header: *header,
            data: Cow::Borrowed(data),
        })?;

        Ok(())
    }

    fn finish(
        self,
    ) -> Result<(CountingCrcWriter<W>, ChunkMode, crate::shared_chunk_index::SharedChunkIndex), (W, McapError)> {
        // Get the number of uncompressed bytes written and the CRC.
        fn unwrap_writer<W>(writer: CountingCrcWriter<ChunkSink<W>>) -> W {
            writer.finalize().0.inner
        }

        let uncompressed_size = self.compressor.position();
        let (stream, uncompressed_crc) = self.compressor.finalize();

        // Finalize the compression stream - it maintains an internal buffer.
        let (writer, result) = stream.finish();

        if let Err(err) = result {
            return Err((unwrap_writer(writer), err.into()));
        }
        let compressed_size = writer.position();
        let (mut sink, compressed_crc) = writer.finalize();
        let data_end = match sink.stream_position() {
            Ok(v) => v,
            Err(err) => return Err((sink.inner, err.into())),
        };
        // let compressed_size =  data_end - self.data_start;
        let record_size = (data_end - self.header_start) - 9; // opcode + record length

        // Now that we know the size of the chunk data and the CRC of the uncompressed data, we
        // rewind the stream and overwrite the dummy chunk header with the true header.
        if let Err(err) = sink.seek(SeekFrom::Start(self.header_start)) {
            return Err((sink.inner, err.into()));
        }
        // Compute the CRC of the pre-chunk data concatenated with the chunk header.
        let mut writer = CountingCrcWriter::with_hasher(sink, self.pre_chunk_crc);

        let message_bounds = self.message_bounds.unwrap_or((0, 0));
        let header = records::ChunkHeaderRef {
            message_start_time: message_bounds.0,
            message_end_time: message_bounds.1,
            uncompressed_size,
            uncompressed_crc: uncompressed_crc
                .map(|hasher| hasher.finalize())
                .unwrap_or(0),
            compression: self.compression_name,
            compressed_size,
        };
        if let Err(err) = serialize_output(&mut writer, |output| {
            op_and_len(output, op::CHUNK, record_size)?;
            NoSeek::new(output).write_le(&header).map_err(io::Error::other)
        }) {
            return Err((unwrap_writer(writer), err));
        }
        let (mut sink, mut post_chunk_header_crc) = writer.finalize();
        let position = match sink.stream_position() {
            Ok(v) => v,
            Err(err) => return Err((sink.inner, err.into())),
        };
        assert_eq!(self.data_start, position);
        // We're done with all the chunk data. Move the cursor past the end and go back to just
        // appending records.
        if let Err(err) = sink.seek(SeekFrom::End(0)) {
            return Err((sink.inner, err.into()));
        }
        let chunk_length = data_end - self.header_start;
        let (writer, mode_result) = sink.finish();
        let mode = match mode_result {
            Ok(mode) => mode,
            Err(err) => return Err((writer, err.into())),
        };

        // Compute the CRC of the pre-chunk data + chunk header + compressed chunk data. That is,
        // the CRC of the entire MCAP file up to the end of this chunk. This is necessary because
        // we ultimately have to produce a correct CRC of the MCAP file until the DataEnd record.
        if let (Some(hasher), Some(compressed_crc)) = (&mut post_chunk_header_crc, &compressed_crc)
        {
            hasher.combine(compressed_crc);
        }
        let mut writer = CountingCrcWriter::with_hasher(writer, post_chunk_header_crc);
        // Write our message indexes
        let data_end = match writer.stream_position() {
            Ok(v) => v,
            Err(err) => {
                return Err((writer.finalize().0, err.into()));
            }
        };
        let mut message_index_offsets = crate::shared_chunk_index::ChunkOffsets::empty();
        for (channel_id, records) in self.indexes {
            let position = match writer.stream_position() {
                Ok(v) => v,
                Err(err) => return Err((writer.finalize().0, err.into())),
            };
            let existing_offset = match message_index_offsets.insert(channel_id, position, &self.index_domain, crate::storage::OwnerKind::Operation) {
                Ok(value) => value,
                Err(error) => return Err((writer.finalize().0, error.into())),
            };
            assert!(existing_offset.is_none());

            let result = (|| -> std::io::Result<()> {
                let bytes = records
                    .len()
                    .checked_mul(16)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| std::io::Error::other("Message index length overflow"))?;
                op_and_len(&mut writer, op::MESSAGE_INDEX, 6 + bytes as u64)?;
                writer.write_all(&channel_id.to_le_bytes())?;
                writer.write_all(&bytes.to_le_bytes())?;
                for record in records.iter() {
                    writer.write_all(&record.log_time.to_le_bytes())?;
                    writer.write_all(&record.offset.to_le_bytes())?;
                }
                Ok(())
            })();
            if let Err(err) = result {
                return Err((writer.finalize().0, err.into()));
            }
        }
        let position = match writer.stream_position() {
            Ok(pos) => pos,
            Err(err) => {
                return Err((writer.finalize().0, err.into()));
            }
        };
        let message_index_length = position - data_end;

        let index = match crate::shared_chunk_index::SharedChunkIndex::new(
            crate::shared_chunk_index::ChunkIndexFields {
                message_start_time: header.message_start_time,
                message_end_time: header.message_end_time,
                chunk_start_offset: self.chunk_offset,
                chunk_length,
            }, message_index_length, header.compression, header.compressed_size,
            header.uncompressed_size, message_index_offsets, &self.index_domain,
            crate::storage::OwnerKind::Operation,
        ) {
            Ok(index) => index,
            Err(error) => return Err((writer.finalize().0, error.into())),
        };

        Ok((writer, mode, index))
    }
}

struct AttachmentWriter<W> {
    record_offset: u64,
    attachment_offset: u64,
    attachment_length: u64,
    index: Option<crate::shared_attachment_index::SharedAttachmentIndex>,
    writer: CountingCrcWriter<W>,
}

impl<W: Write + Seek> AttachmentWriter<W> {
    /// Create a new [`AttachmentWriter`] and write the attachment header to the output.
    fn new(
        mut writer: W,
        attachment_length: u64,
        header: records::AttachmentHeaderRef<'_>,
        calculate_crc: bool,
        emit_index: bool,
        domain: &crate::storage::BudgetRef,
    ) -> Result<Self, (W, McapError)> {
        let prepared = (|| -> McapResult<_> {
            let record_offset = writer.stream_position()?;
            let length = serialized_body_len(&header)?
                .checked_add(size_of::<u64>() as u64)
                .and_then(|n| n.checked_add(attachment_length))
                .and_then(|n| n.checked_add(size_of::<u32>() as u64))
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
            let index = if emit_index {
                Some(crate::shared_attachment_index::SharedAttachmentIndex::new(
                    record_offset,
                    0,
                    header.log_time,
                    header.create_time,
                    attachment_length,
                    header.name,
                    header.media_type,
                    domain,
                    crate::storage::OwnerKind::Operation,
                )?)
            } else {
                None
            };
            Ok((record_offset, length, index))
        })();
        let (record_offset, length, index) = match prepared {
            Ok(value) => value,
            Err(error) => return Err((writer, error)),
        };
        if let Err(error) = op_and_len(&mut writer, op::ATTACHMENT, length) {
            return Err((writer, error.into()));
        }
        let mut writer = CountingCrcWriter::new(writer, calculate_crc);
        let result = (|| -> McapResult<()> {
            NoSeek::new(&mut writer).write_le(&header)?;
            writer.write_u64::<LE>(attachment_length)?;
            Ok(())
        })();
        if let Err(error) = result {
            return Err((writer.finalize().0, error));
        }
        let attachment_offset = writer.position();
        Ok(Self {
            record_offset,
            attachment_offset,
            attachment_length,
            index,
            writer,
        })
    }

    /// Write bytes to the attachment.
    ///
    /// This method will return an error if the provided bytes exceed the space remaining in the
    /// attachment.
    fn put_bytes(&mut self, bytes: &[u8]) -> McapResult<()> {
        let attachment_position = self.writer.position() - self.attachment_offset;

        let space = self.attachment_length - attachment_position;
        let byte_length = bytes.len() as u64;

        if byte_length > space {
            return Err(McapError::AttachmentTooLarge {
                excess: byte_length - space,
                attachment_length: self.attachment_length,
            });
        }

        self.writer.write_all(bytes)?;
        Ok(())
    }

    /// Finish the attachment and write the CRC to the output, returning the [`records::AttachmentIndex`]
    /// for the written attachment.
    fn finish(
        mut self,
    ) -> Result<
        (
            W,
            Option<crate::shared_attachment_index::SharedAttachmentIndex>,
        ),
        (W, McapError),
    > {
        let expected = self.attachment_length;
        let current = self.writer.position() - self.attachment_offset;
        if expected != current {
            return Err((
                self.writer.finalize().0,
                McapError::AttachmentIncomplete { expected, current },
            ));
        }
        let (mut writer, hasher) = self.writer.finalize();
        let crc = hasher.map(|hasher| hasher.finalize()).unwrap_or(0);
        let result = (|| -> McapResult<()> {
            writer.write_u32::<LE>(crc)?;
            let length = writer.stream_position()? - self.record_offset;
            if let Some(index) = &mut self.index {
                index.finish_length(length);
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok((writer, self.index)),
            Err(error) => Err((writer, error)),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn buffered_chunk_header_refusal_preserves_typed_failure() {
        use crate::storage::{BudgetLimits, BudgetRef, ResourceCategory, StorageFailureKind};
        for block in [16, 4096] {
            let domain = BudgetRef::new(BudgetLimits {block, ..Default::default()}).unwrap();
            let mode = ChunkMode::Buffered {
                buffer: Vec::new(), charge: domain.reserve_class(0, ResourceCategory::Writer).unwrap(),
            };
            if block == 4096 { domain.fail_allocation_at(0); }
            let result = ChunkWriter::new(CountingCrcWriter::new(Cursor::new(Vec::new()), true),
                None, mode, false, domain.clone(), true,
                #[cfg(any(feature = "zstd", feature = "lz4"))] 0,
                #[cfg(feature = "zstd")] 0);
            let Err(McapError::Storage(error)) = result else { panic!("expected fixed storage failure") };
            assert!(error.terminal);
            if block == 16 {
                assert_eq!(error.kind, StorageFailureKind::PermanentLimit);
                assert_eq!(error.details.limit, 16);
                assert_eq!(error.details.domain_limit, domain.limits().total);
                assert_eq!(error.details.resource, "WriterBuffer");
                assert_eq!(error.details.phase, "chunk-growth");
            } else { assert_eq!(error.kind, StorageFailureKind::SystemAllocation); }
            assert_eq!(domain.workload_statistics().current, 0);
        }
    }

    #[test]
    fn record_sink_preserves_short_writes_and_first_failure() {
        struct Output {
            bytes: Vec<u8>,
            calls: usize,
            stop: Option<io::ErrorKind>,
        }
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == 1 { return Err(io::ErrorKind::Interrupted.into()); }
                if self.bytes.len() >= 4 {
                    if let Some(kind) = self.stop {
                        return if kind == io::ErrorKind::WriteZero { Ok(0) } else { Err(kind.into()) };
                    }
                }
                let count = bytes.len().min(2);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.calls += 1;
                Err(io::ErrorKind::PermissionDenied.into())
            }
        }
        impl McapWrite for Output {
            fn write_mcap(&mut self, bytes: &[u8]) -> McapResult<usize> { Ok(self.write(bytes)?) }
            fn flush_mcap(&mut self) -> McapResult<()> { Ok(self.flush()?) }
        }
        for stop in [None, Some(io::ErrorKind::WriteZero), Some(io::ErrorKind::BrokenPipe)] {
            let output = Output { bytes: Vec::new(), calls: 0, stop };
            let mut compressor = CountingCrcWriter::new(Compressor::Null(output), true);
            let mut sink = RecordSink { compressor: &mut compressor, failure: None };
            sink.write_all(b"abcdefgh").unwrap();
            if let Some(expected) = stop {
                assert!(matches!(&sink.failure, Some(McapError::Io(error)) if error.kind() == expected));
                sink.write_all(b"ignored").unwrap();
                sink.flush().unwrap();
                assert!(matches!(&sink.failure, Some(McapError::Io(error)) if error.kind() == expected));
            } else {
                assert!(sink.failure.is_none());
                sink.flush().unwrap();
                assert!(matches!(&sink.failure, Some(McapError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied));
            }
            let expected = if stop.is_some() { &b"abcd"[..] } else { &b"abcdefgh"[..] };
            assert_eq!(compressor.position(), expected.len() as u64);
            assert_eq!(compressor.current_checksum(), crc32fast::hash(expected));
            let (Compressor::Null(output), _) = compressor.finalize() else { unreachable!() };
            assert_eq!(output.bytes, expected);
            assert_eq!(output.calls, if stop.is_some() { 4 } else { 6 });
        }
    }

    #[test]
    fn all_summary_declaration_allocation_refusals_are_terminal_and_releasable() {
        fn setup(domain: &crate::storage::BudgetRef) -> Writer<std::io::Cursor<Vec<u8>>> {
            let mut writer=WriteOptions::new().compression(None).use_chunks(false).memory_budget(domain.clone()).create(std::io::Cursor::new(Vec::new())).unwrap();
            let schema=writer.add_schema("schema","raw",&[7;1024]).unwrap();
            writer.add_channel(schema,"topic","raw",&[("a".into(),"one".into()),("z".into(),"two".into())].into()).unwrap();
            writer
        }
        let domain=crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer=setup(&domain);
        let before=domain.workload_detailed_statistics().allocation_count;
        let summary=writer.finish().unwrap();
        let count=domain.workload_detailed_statistics().allocation_count-before;
        assert!(count>=12);
        let duplicate=summary.clone();drop(writer);drop(summary);
        assert_eq!(duplicate.channels[&1].metadata.get("z").unwrap(),"two");
        assert_eq!(duplicate.schemas[&1].data.as_ref(),&[7;1024]);
        drop(duplicate);assert_eq!(domain.workload_statistics().current,0);
        for fail in 0..count {
            let domain=crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut writer=setup(&domain);domain.fail_allocation_at(fail as usize);
            assert!(writer.finish().is_err());
            assert!(matches!(writer.finish(),Err(McapError::AttemptedWriteAfterFailure)));
            drop(writer);assert_eq!(domain.workload_statistics().current,0);assert_eq!(domain.ownership_statistics(),Default::default());
        }
    }
    #[test]
    fn default_options_defer_unused_storage_and_preserve_thread_selection() {
        let options = WriteOptions::new();
        assert!(options.memory_budget.is_none());
        assert!(matches!(options.profile, crate::option_text::Text::Borrowed("")));
        assert!(matches!(options.library, crate::option_text::Text::Borrowed(crate::LIBRARY_IDENTIFIER)));
        #[cfg(feature = "zstd")]
        {
            assert_eq!(options.compression_threads, None);
            let writer = options.clone().create(std::io::Cursor::new(Vec::new())).unwrap();
            assert_eq!(writer.options.compression_threads, Some(num_cpus::get_physical() as u32));
            let writer = options.clone().compression_threads(0).create(std::io::Cursor::new(Vec::new())).unwrap();
            assert_eq!(writer.options.compression_threads, Some(0));
            let writer = options.clone().compression_threads(2).create(std::io::Cursor::new(Vec::new())).unwrap();
            assert_eq!(writer.options.compression_threads, Some(2));
        }
    }
    #[test]
    fn chunk_index_allocation_refusals_leave_writer_terminal() {
        for compression in [None, Some(crate::Compression::Lz4), Some(crate::Compression::Zstd)] {
            fn setup(compression: Option<crate::Compression>, domain: &crate::storage::BudgetRef) -> super::Writer<std::io::Cursor<Vec<u8>>> {
                let mut writer = super::WriteOptions::new().compression(compression).chunk_size(None)
                    .memory_budget(domain.clone()).create(std::io::Cursor::new(Vec::new())).unwrap();
                for id in [1, 255, 256, 512, 65535] {
                    writer.add_channel_with_id(id, 0, "topic", "raw", &Default::default()).unwrap();
                    writer.write_to_known_channel(&crate::records::MessageHeader { channel_id:id, sequence:0, log_time:0, publish_time:0 }, &[1,2,3]).unwrap();
                }
                writer
            }
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let mut writer = setup(compression, &domain);
            let before = domain.workload_detailed_statistics().allocation_count;
            writer.finish_chunk().unwrap();
            let calls = domain.workload_detailed_statistics().allocation_count - before;
            assert!(calls >= 6);
            drop(writer);
            assert_eq!(domain.workload_statistics().current, 0);
            for fail in 0..calls {
                let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
                let mut writer = setup(compression, &domain);
                domain.fail_allocation_at(fail as usize);
                assert!(writer.finish_chunk().is_err(), "{compression:?}/{fail}");
                assert!(matches!(writer.finish(), Err(crate::McapError::AttemptedWriteAfterFailure)));
                drop(writer);
                assert_eq!(domain.workload_statistics().current, 0, "{compression:?}/{fail}");
                assert_eq!(domain.ownership_statistics(), Default::default());
            }
        }
    }
    #[test]
    fn attachment_io_failures_return_output_and_release_final_index_storage() {
        struct Limited {
            position: u64,
            limit: u64,
        }
        impl std::io::Write for Limited {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                let n = data.len().min((self.limit - self.position) as usize);
                if n == 0 {
                    return Err(std::io::ErrorKind::Other.into());
                }
                self.position += n as u64;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl std::io::Seek for Limited {
            fn seek(&mut self, offset: std::io::SeekFrom) -> std::io::Result<u64> {
                match offset {
                    std::io::SeekFrom::Current(0) => Ok(self.position),
                    _ => Err(std::io::ErrorKind::Unsupported.into()),
                }
            }
        }
        // 9-byte record prefix + 31-byte header + 8-byte data length + 3 payload + 4 CRC.
        for limit in 0..=55 {
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let header = records::AttachmentHeaderRef {
                log_time: 1,
                create_time: 2,
                name: "name",
                media_type: "raw",
            };
            match AttachmentWriter::new(
                Limited { position: 0, limit },
                3,
                header,
                true,
                true,
                &domain,
            ) {
                Err((output, _)) => assert_eq!(output.position, limit),
                Ok(mut attachment) => {
                    let _ = attachment.put_bytes(&[1, 2, 3]);
                    match attachment.finish() {
                        Err((output, _)) => {
                            assert!(limit < 55);
                            assert_eq!(output.position, limit);
                        }
                        Ok((output, index)) => {
                            assert_eq!(limit, 55);
                            assert_eq!(output.position, 55);
                            let index = index.unwrap();
                            assert_eq!(index.length, 55);
                            assert_eq!(index.name, "name");
                        }
                    }
                }
            }
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    #[test]
    fn summary_directory_refusal_is_terminal() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer = super::WriteOptions::default()
            .use_chunks(false)
            .memory_budget(domain.clone())
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        writer
            .add_channel(0, "topic", "raw", &Default::default())
            .unwrap();
        domain.fail_allocation_at(0);
        assert!(writer.finish().is_err());
        assert!(matches!(
            writer.finish(),
            Err(crate::McapError::AttemptedWriteAfterFailure)
        ));
        drop(writer);
        assert_eq!(domain.workload_statistics().current, 0);
    }

    #[test]
    fn extracting_unfinished_stream_does_not_write_summary_or_footer() {
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer = super::WriteOptions::default()
            .use_chunks(false)
            .memory_budget(domain.clone())
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        writer
            .add_channel(0, "topic", "raw", &Default::default())
            .unwrap();
        // Extraction remains valid even when no further budget allocation can succeed.
        domain.fail_allocation_at(0);
        let bytes = writer.into_inner().into_inner();
        assert!(bytes.starts_with(crate::MAGIC));
        assert!(!bytes.ends_with(crate::MAGIC));
        assert_eq!(domain.workload_statistics().current, 0);
    }
    use assert_matches::assert_matches;
    use std::sync::Arc;

    use crate::read::LinearReader;

    use super::*;

    const DEFAULT_LIBRARY_LENGTH: u64 = crate::LIBRARY_IDENTIFIER.len() as u64;

    #[test]
    fn writes_all_channel_ids() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let custom_channel = Arc::new(crate::Channel {
            id: u16::MAX,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: None,
        });
        writer
            .write(&crate::Message {
                channel: custom_channel.clone(),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Cow::Owned(Vec::new()),
            })
            .expect("could not write initial channel");
        for i in 1..65535u16 {
            let id = writer
                .add_channel(0, &format!("{i}"), "json", &BTreeMap::new())
                .expect("could not add channel");
            assert_eq!(i, id);
        }
        let Err(too_many) = writer.add_channel(0, "last", "json", &BTreeMap::new()) else {
            panic!("should not be able to add another channel");
        };
        assert!(matches!(too_many, McapError::TooManyChannels));
    }
    #[test]
    fn writes_all_schema_ids() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let custom_channel = Arc::new(crate::Channel {
            id: 0,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: Some(Arc::new(crate::Schema {
                id: u16::MAX,
                name: "int".into(),
                encoding: "jsonschema".into(),
                data: Cow::Owned(Vec::new()),
            })),
        });
        writer
            .write(&crate::Message {
                channel: custom_channel.clone(),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Cow::Owned(Vec::new()),
            })
            .expect("could not write initial channel");
        for i in 0..65534u16 {
            let id = writer
                .add_schema(&format!("{i}"), "jsonschema", &[])
                .expect("could not add schema");
            assert_eq!(id, i + 1);
        }
        let Err(too_many) = writer.add_schema("last", "jsonschema", &[]) else {
            panic!("should not be able to add another channel");
        };
        assert!(matches!(too_many, McapError::TooManySchemas));
    }

    #[test]
    fn summary_contains_all_ids() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_a = Arc::new(crate::Schema {
            id: u16::MAX,
            name: "int".into(),
            encoding: "jsonschema".into(),
            data: Cow::Owned(Vec::new()),
        });
        let schema_b = Arc::new(crate::Schema {
            id: u16::MAX,
            name: "int".into(),
            encoding: "jsonschema".into(),
            data: Cow::Owned(Vec::new()),
        });
        let channel_a = Arc::new(crate::Channel {
            id: 0,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: Some(schema_a.clone()),
        });
        let channel_b = Arc::new(crate::Channel {
            id: 1,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: Some(schema_a.clone()),
        });
        let channel_c = Arc::new(crate::Channel {
            id: 2,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: Some(schema_b.clone()),
        });

        let data: &[u8] = &[0, 1, 2, 3];

        writer
            .write(&Message {
                channel: channel_a.clone(),
                sequence: 1,
                log_time: 1,
                publish_time: 1,
                data: Cow::Borrowed(data),
            })
            .expect("failed write 1");
        writer
            .write(&Message {
                channel: channel_b.clone(),
                sequence: 2,
                log_time: 2,
                publish_time: 2,
                data: Cow::Borrowed(data),
            })
            .expect("failed write 2");
        writer
            .write(&Message {
                channel: channel_c.clone(),
                sequence: 3,
                log_time: 3,
                publish_time: 3,
                data: Cow::Borrowed(data),
            })
            .expect("failed write 3");
        let summary = writer.finish().expect("failed to finish");
        let statistics = summary.stats.unwrap();
        assert_eq!(statistics.channel_message_counts.get(&0), Some(&1));
        assert_eq!(statistics.channel_message_counts.get(&1), Some(&1));
        assert_eq!(statistics.channel_message_counts.get(&2), Some(&1));
        assert_eq!(statistics.message_count, 3);
        assert_eq!(statistics.metadata_count, 0);
        assert_eq!(statistics.attachment_count, 0);
        assert_eq!(statistics.chunk_count, 1);
        assert!(summary.attachment_indexes.is_empty());
        assert!(summary.metadata_indexes.is_empty());
        assert_eq!(summary.chunk_indexes.len(), 1);
    }

    #[test]
    #[should_panic(expected = "Trying to write a record on a finished MCAP")]
    fn panics_if_write_called_after_finish() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        writer.finish().expect("failed to finish writer");

        let custom_channel = Arc::new(crate::Channel {
            id: 1,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: None,
        });

        writer
            .write(&crate::Message {
                channel: custom_channel.clone(),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Cow::Owned(Vec::new()),
            })
            .expect("could not write message");
    }

    #[test]
    fn writes_message_and_checks_stream_length() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let custom_channel = Arc::new(crate::Channel {
            id: 1,
            topic: "chat".into(),
            message_encoding: "json".into(),
            metadata: BTreeMap::new(),
            schema: None,
        });

        writer
            .write(&crate::Message {
                channel: custom_channel.clone(),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Cow::Owned(Vec::new()),
            })
            .expect("could not write message");

        writer.finish().expect("failed to finish writer");

        let output_len = writer
            .into_inner()
            .stream_position()
            .expect("failed to get stream position");
        assert_eq!(output_len, 473 + DEFAULT_LIBRARY_LENGTH);
    }

    #[test]
    fn preserves_written_channel_ids_in_write() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema = Arc::new(crate::Schema {
            id: 1,
            name: "schema".into(),
            encoding: "ros1msg".into(),
            data: Vec::new().into(),
        });
        let first_channel = crate::Channel {
            id: 1,
            topic: "chat".into(),
            schema: Some(schema.clone()),
            message_encoding: "ros1".into(),
            metadata: Default::default(),
        };
        let second_channel = crate::Channel {
            id: 2,
            schema: Some(schema.clone()),
            ..first_channel.clone()
        };
        let third_channel = crate::Channel {
            id: 3,
            schema: Some(schema.clone()),
            ..first_channel.clone()
        };
        writer
            .write(&crate::Message {
                channel: Arc::new(first_channel),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Vec::new().into(),
            })
            .expect("failed to write first message");
        writer
            .write(&crate::Message {
                channel: Arc::new(second_channel),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Vec::new().into(),
            })
            .expect("failed to write first message");
        writer
            .write(&crate::Message {
                channel: Arc::new(third_channel),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Vec::new().into(),
            })
            .expect("failed to write first message");

        writer.finish().expect("failed in finish");
        let buf = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&buf)
            .expect("failed to parse summary")
            .expect("expected a summary");
        assert_eq!(summary.channels.len(), 3);
        assert_eq!(summary.schemas.len(), 1);
    }

    #[test]
    fn deduplicated_ids_in_add_schema_channel() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let first_schema_id = writer
            .add_schema("first", "ros1msg", &[1, 2, 3])
            .expect("failed to write schema");
        assert_eq!(
            writer
                .add_schema("first", "ros1msg", &[1, 2, 3])
                .expect("failed to write schema"),
            first_schema_id
        );
        let second_schema_id = writer
            .add_schema("second", "ros1msg", &[1, 2, 3])
            .expect("failed to write schema");
        assert_ne!(first_schema_id, second_schema_id);

        let first_channel_id = writer
            .add_channel(first_schema_id, "a", "enc", &BTreeMap::new())
            .expect("failed to write channel");
        assert_eq!(
            writer
                .add_channel(first_schema_id, "a", "enc", &BTreeMap::new())
                .expect("failed to write channel"),
            first_channel_id
        );
        let second_channel_id = writer
            .add_channel(first_schema_id, "b", "enc", &BTreeMap::new())
            .expect("failed to write channel");

        writer
            .write_to_known_channel(
                &MessageHeader {
                    channel_id: first_channel_id,
                    sequence: 0,
                    log_time: 0,
                    publish_time: 0,
                },
                &[1, 2, 3],
            )
            .expect("failed to write message");
        writer
            .write_to_known_channel(
                &MessageHeader {
                    channel_id: second_channel_id,
                    sequence: 1,
                    log_time: 1,
                    publish_time: 1,
                },
                &[1, 2, 3],
            )
            .expect("failed to write message");

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.channels.len(), 2);
        assert_eq!(summary.schemas.len(), 2);
    }

    #[test]
    fn add_schema_with_id_preserves_duplicate_content_schemas() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let schema_one = writer
            .add_schema_with_id(10, "schema", "jsonschema", br#"{}"#)
            .expect("failed to add first schema");
        let schema_two = writer
            .add_schema_with_id(20, "schema", "jsonschema", br#"{}"#)
            .expect("failed to add second schema");

        assert_eq!(schema_one, 10);
        assert_eq!(schema_two, 20);
        assert_ne!(schema_one, schema_two);

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.schemas.len(), 2);
    }

    #[test]
    fn add_schema_with_id_rejects_conflicting_existing_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        writer
            .add_schema_with_id(7, "schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let err = writer
            .add_schema_with_id(7, "schema", "jsonschema", br#"{"type":"object"}"#)
            .expect_err("conflicting schema id should fail");
        assert_matches!(err, McapError::ConflictingSchemas(_));
    }

    #[test]
    fn add_schema_with_id_is_idempotent_for_same_id_and_content() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let first = writer
            .add_schema_with_id(7, "schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");
        let second = writer
            .add_schema_with_id(7, "schema", "jsonschema", br#"{}"#)
            .expect("failed to re-add identical schema");
        assert_eq!(first, second);

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.schemas.len(), 1);
    }

    #[test]
    fn add_schema_with_id_rejects_zero_schema_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let err = writer
            .add_schema_with_id(0, "schema", "jsonschema", br#"{}"#)
            .expect_err("schema id 0 should fail");
        assert_matches!(err, McapError::InvalidSchemaId);
    }

    #[test]
    fn add_schema_with_id_sets_canonical_id_for_add_schema() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let explicit = writer
            .add_schema_with_id(9000, "schema", "jsonschema", br#"{}"#)
            .expect("failed to add explicit schema");
        let auto = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add automatic schema");

        assert_eq!(explicit, 9000);
        assert_eq!(auto, 9000);
    }

    #[test]
    fn add_schema_with_id_does_not_advance_allocator() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let explicit_id = writer
            .add_schema_with_id(9000, "explicit", "jsonschema", br#"{}"#)
            .expect("failed to add explicit schema");
        assert_eq!(explicit_id, 9000);

        let first_auto = writer
            .add_schema("auto1", "jsonschema", br#"{}"#)
            .expect("failed to add first automatic schema");
        let second_auto = writer
            .add_schema("auto2", "jsonschema", br#"{}"#)
            .expect("failed to add second automatic schema");

        assert_eq!(first_auto, 1);
        assert_eq!(second_auto, 2);
    }

    #[test]
    fn add_schema_with_id_keeps_allocator_at_lowest_free_id_with_interleaving() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        let auto_1 = writer
            .add_schema("auto1", "jsonschema", br#"{}"#)
            .expect("failed to add first auto schema");
        let auto_2 = writer
            .add_schema("auto2", "jsonschema", br#"{}"#)
            .expect("failed to add second auto schema");
        let explicit_9000 = writer
            .add_schema_with_id(9000, "explicit9000", "jsonschema", br#"{}"#)
            .expect("failed to add explicit 9000 schema");
        let auto_3 = writer
            .add_schema("auto3", "jsonschema", br#"{}"#)
            .expect("failed to add third auto schema");
        let explicit_5 = writer
            .add_schema_with_id(5, "explicit5", "jsonschema", br#"{}"#)
            .expect("failed to add explicit 5 schema");
        let auto_4 = writer
            .add_schema("auto4", "jsonschema", br#"{}"#)
            .expect("failed to add fourth auto schema");
        let auto_6 = writer
            .add_schema("auto6", "jsonschema", br#"{}"#)
            .expect("failed to add fifth auto schema");

        assert_eq!(auto_1, 1);
        assert_eq!(auto_2, 2);
        assert_eq!(explicit_9000, 9000);
        assert_eq!(auto_3, 3);
        assert_eq!(explicit_5, 5);
        assert_eq!(auto_4, 4);
        assert_eq!(auto_6, 6);
    }

    #[test]
    fn add_schema_with_id_max_keeps_allocator_progress() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");

        writer
            .add_schema_with_id(u16::MAX, "max", "jsonschema", br#"{}"#)
            .expect("failed to add max-id schema");
        let auto_id = writer
            .add_schema("auto", "jsonschema", br#"{}"#)
            .expect("failed to add automatic schema after max-id schema");
        assert_eq!(auto_id, 1);
    }

    #[test]
    fn add_channel_with_id_preserves_duplicate_content_channels() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let channel_one = writer
            .add_channel_with_id(10, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add channel one");
        let channel_two = writer
            .add_channel_with_id(20, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add channel two");

        assert_eq!(channel_one, 10);
        assert_eq!(channel_two, 20);
        assert_ne!(channel_one, channel_two);

        writer
            .write_to_known_channel(
                &MessageHeader {
                    channel_id: channel_one,
                    sequence: 0,
                    log_time: 1,
                    publish_time: 1,
                },
                &[1],
            )
            .expect("failed to write first message");
        writer
            .write_to_known_channel(
                &MessageHeader {
                    channel_id: channel_two,
                    sequence: 0,
                    log_time: 2,
                    publish_time: 2,
                },
                &[2],
            )
            .expect("failed to write second message");

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.channels.len(), 2);
    }

    #[test]
    fn add_channel_with_id_rejects_conflicting_existing_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        writer
            .add_channel_with_id(7, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add channel");

        let err = writer
            .add_channel_with_id(7, schema_id, "/other", "json", &BTreeMap::new())
            .expect_err("conflicting channel id should fail");
        assert_matches!(err, McapError::ConflictingChannels(_));
    }

    #[test]
    fn add_channel_with_id_is_idempotent_for_same_id_and_content() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let first = writer
            .add_channel_with_id(7, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add channel");
        let second = writer
            .add_channel_with_id(7, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to re-add identical channel");
        assert_eq!(first, second);

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.channels.len(), 1);
    }

    #[test]
    fn add_channel_with_id_allows_zero_channel_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let explicit = writer
            .add_channel_with_id(0, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add explicit channel 0");
        assert_eq!(explicit, 0);

        let first_auto = writer
            .add_channel(schema_id, "/auto", "json", &BTreeMap::new())
            .expect("failed to add automatic channel");
        assert_eq!(first_auto, 1);
    }

    #[test]
    fn add_channel_with_id_sets_canonical_id_for_add_channel() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let explicit = writer
            .add_channel_with_id(9000, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add explicit channel");
        let auto = writer
            .add_channel(schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add automatic channel");

        assert_eq!(explicit, 9000);
        assert_eq!(auto, 9000);
    }

    #[test]
    fn add_channel_with_id_does_not_advance_allocator() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let explicit_id = writer
            .add_channel_with_id(9000, schema_id, "/explicit", "json", &BTreeMap::new())
            .expect("failed to add explicit channel");
        assert_eq!(explicit_id, 9000);

        let first_auto = writer
            .add_channel(schema_id, "/auto1", "json", &BTreeMap::new())
            .expect("failed to add first automatic channel");
        let second_auto = writer
            .add_channel(schema_id, "/auto2", "json", &BTreeMap::new())
            .expect("failed to add second automatic channel");

        assert_eq!(first_auto, 1);
        assert_eq!(second_auto, 2);
    }

    #[test]
    fn add_channel_with_id_keeps_allocator_at_lowest_free_id_with_interleaving() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let auto_1 = writer
            .add_channel(schema_id, "/auto1", "json", &BTreeMap::new())
            .expect("failed to add first auto channel");
        let auto_2 = writer
            .add_channel(schema_id, "/auto2", "json", &BTreeMap::new())
            .expect("failed to add second auto channel");
        let explicit_9000 = writer
            .add_channel_with_id(9000, schema_id, "/explicit9000", "json", &BTreeMap::new())
            .expect("failed to add explicit 9000 channel");
        let auto_3 = writer
            .add_channel(schema_id, "/auto3", "json", &BTreeMap::new())
            .expect("failed to add third auto channel");
        let explicit_5 = writer
            .add_channel_with_id(5, schema_id, "/explicit5", "json", &BTreeMap::new())
            .expect("failed to add explicit 5 channel");
        let auto_4 = writer
            .add_channel(schema_id, "/auto4", "json", &BTreeMap::new())
            .expect("failed to add fourth auto channel");
        let auto_6 = writer
            .add_channel(schema_id, "/auto6", "json", &BTreeMap::new())
            .expect("failed to add fifth auto channel");

        assert_eq!(auto_1, 1);
        assert_eq!(auto_2, 2);
        assert_eq!(explicit_9000, 9000);
        assert_eq!(auto_3, 3);
        assert_eq!(explicit_5, 5);
        assert_eq!(auto_4, 4);
        assert_eq!(auto_6, 6);
    }

    #[test]
    fn add_channel_with_id_max_keeps_allocator_progress() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        writer
            .add_channel_with_id(u16::MAX, schema_id, "/max", "json", &BTreeMap::new())
            .expect("failed to add max-id channel");
        let auto_id = writer
            .add_channel(schema_id, "/auto", "json", &BTreeMap::new())
            .expect("failed to add automatic channel after max-id channel");
        assert_eq!(auto_id, 1);
    }

    #[test]
    fn add_channel_with_id_is_idempotent_for_matching_existing_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        let first = writer
            .add_channel_with_id(7, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add channel");
        let second = writer
            .add_channel_with_id(7, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("re-adding same id/content should be ok");

        assert_eq!(first, 7);
        assert_eq!(second, 7);

        writer.finish().expect("failed to finish");
        let mcap = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&mcap)
            .expect("failed to read summary")
            .expect("summary should be present");
        assert_eq!(summary.channels.len(), 1);
    }

    #[test]
    fn add_channel_with_id_keeps_auto_allocator_at_lowest_free_id() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let schema_id = writer
            .add_schema("schema", "jsonschema", br#"{}"#)
            .expect("failed to add schema");

        writer
            .add_channel_with_id(9000, schema_id, "/topic", "json", &BTreeMap::new())
            .expect("failed to add explicit channel");
        let auto_id = writer
            .add_channel(schema_id, "/auto", "json", &BTreeMap::new())
            .expect("failed to add auto channel");
        assert_eq!(auto_id, 1);

        writer
            .add_channel_with_id(u16::MAX, schema_id, "/max", "json", &BTreeMap::new())
            .expect("failed to add explicit max id");
        let next_auto_id = writer
            .add_channel(schema_id, "/next-auto", "json", &BTreeMap::new())
            .expect("failed to add next auto channel");
        assert_eq!(next_auto_id, 2);
    }

    #[test]
    fn preserves_written_schema_ids_in_write() {
        let file = std::io::Cursor::new(Vec::new());
        let mut writer = Writer::new(file).expect("failed to construct writer");
        let first_schema = crate::Schema {
            id: 1,
            name: "schema".into(),
            encoding: "ros1msg".into(),
            data: Vec::new().into(),
        };
        let second_schema = crate::Schema {
            id: 2,
            ..first_schema.clone()
        };
        let first_channel = crate::Channel {
            id: 1,
            topic: "chat".into(),
            schema: Some(Arc::new(first_schema)),
            message_encoding: "ros1".into(),
            metadata: Default::default(),
        };
        let second_channel = crate::Channel {
            id: 2,
            schema: Some(Arc::new(second_schema)),
            ..first_channel.clone()
        };
        writer
            .write(&crate::Message {
                channel: Arc::new(first_channel),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Vec::new().into(),
            })
            .expect("failed to write first message");
        writer
            .write(&crate::Message {
                channel: Arc::new(second_channel),
                sequence: 0,
                log_time: 0,
                publish_time: 0,
                data: Vec::new().into(),
            })
            .expect("failed to write first message");

        writer.finish().expect("failed in finish");
        let buf = writer.into_inner().into_inner();
        let summary = crate::Summary::read(&buf)
            .expect("failed to parse summary")
            .expect("expected a summary");
        assert_eq!(summary.channels.len(), 2);
        assert_eq!(summary.schemas.len(), 2);
    }

    #[test]
    fn test_writes_private_record_to_chunk() {
        let mut file = vec![];

        let mut writer = WriteOptions::new()
            .use_chunks(true)
            .compression(None)
            .create(Cursor::new(&mut file))
            .expect("failed to construct writer");

        writer
            .write_private_record(
                0x81,
                b"this is in a chunk",
                PrivateRecordOptions::IncludeInChunks.into(),
            )
            .expect("failed to write");

        writer
            .write_private_record(0x82, b"this is not in a chunk", Default::default())
            .expect("failed to write");

        drop(writer);

        let mut reader = LinearReader::new(&file[..]).expect("failed to construct reader");

        let Record::Header(_) = reader.next().unwrap().unwrap() else {
            panic!("expected header for first record");
        };

        let Record::Chunk { data, .. } = reader.next().unwrap().unwrap() else {
            panic!("expected chunk for next record");
        };

        let mut chunk_reader = LinearReader::sans_magic(&data[..]);

        let Record::Unknown { opcode, data } = chunk_reader.next().unwrap().unwrap() else {
            panic!("expected chunk to contain unknown record");
        };

        assert_eq!(opcode, 0x81);
        assert_eq!(String::from_utf8_lossy(&data[..]), "this is in a chunk");

        let Record::Unknown { opcode, data } = reader.next().unwrap().unwrap() else {
            panic!("expected chunk for next record");
        };

        assert_eq!(opcode, 0x82);
        assert_eq!(String::from_utf8_lossy(&data[..]), "this is not in a chunk");
    }

    #[test]
    fn test_invalid_private_record_opcode_fails() {
        let mut file = vec![];

        let mut writer = WriteOptions::new()
            .use_chunks(true)
            .compression(None)
            .create(Cursor::new(&mut file))
            .expect("failed to construct writer");

        let e = writer
            .write_private_record(
                0x1,
                &[1, 2, 3, 4],
                PrivateRecordOptions::IncludeInChunks.into(),
            )
            .expect_err("should return err");

        assert_eq!(
            e.to_string(),
            "Private records must have an opcode >= 0x80, got 0x01"
        );
    }

    #[test]
    fn test_write_failure_does_not_cause_panic() {
        #[derive(Default)]
        struct FailingWriter {
            fail: std::rc::Rc<std::cell::Cell<bool>>,
        }

        impl std::io::Write for FailingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.fail.get() {
                    return Err(std::io::Error::other("writes now fail"));
                }
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let fail = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut writer = WriteOptions::new()
            .disable_seeking(true)
            .use_chunks(true)
            .chunk_size(Some(10))
            .create(NoSeek::new(FailingWriter { fail: fail.clone() }))
            .expect("writer should construct");

        let message = Message {
            channel: Arc::new(Channel {
                id: 0,
                topic: "chat".into(),
                schema: None,
                message_encoding: "json".into(),
                metadata: Default::default(),
            }),
            sequence: 0,
            log_time: 0,
            publish_time: 0,
            data: Cow::Borrowed(b"hello"),
        };
        writer.write(&message).expect("first should not fail");
        fail.set(true);
        assert_matches!(writer.write(&message), Err(McapError::Io(_)));
        assert_matches!(
            writer.write(&message),
            Err(McapError::AttemptedWriteAfterFailure)
        );
    }
}

#[cfg(test)]
mod borrowed_channel_tests {
    use super::*;
    #[test]
    fn borrowed_channel_ordering_refusal_does_not_advance_output_or_ids() {
        let mut writer = WriteOptions::new()
            .use_chunks(false)
            .create(Cursor::new(Vec::new()))
            .unwrap();
        let position = writer.finish_chunk().unwrap().stream_position().unwrap();
        for pairs in [[("z", "one"), ("a", "two")], [("a", "one"), ("a", "two")]] {
            assert!(writer
                .add_channel_borrowed(0, "topic", "raw", pairs.into_iter())
                .is_err());
            assert!(writer
                .add_channel_with_id_borrowed(42, 0, "topic", "raw", pairs.into_iter())
                .is_err());
            assert_eq!(
                writer.finish_chunk().unwrap().stream_position().unwrap(),
                position
            );
        }
        assert_eq!(
            writer
                .add_channel_borrowed(0, "topic", "raw", [("a", "one"), ("z", "two")].into_iter())
                .unwrap(),
            1
        );
        writer.finish().unwrap();
    }
    #[test]
    fn borrowed_and_owned_channel_calls_produce_identical_recording() {
        let fields = BTreeMap::from([
            ("a".to_string(), "one".to_string()),
            ("z".to_string(), "two".to_string()),
        ]);
        let recording = |borrowed| {
            let mut writer = WriteOptions::new()
                .use_chunks(false)
                .create(Cursor::new(Vec::new()))
                .unwrap();
            if borrowed {
                assert_eq!(
                    writer
                        .add_channel_with_id_borrowed(
                            42,
                            0,
                            "topic",
                            "raw",
                            fields.iter().map(|(k, v)| (k.as_str(), v.as_str()))
                        )
                        .unwrap(),
                    42
                );
                assert_eq!(
                    writer
                        .add_channel_borrowed(
                            0,
                            "topic",
                            "raw",
                            fields.iter().map(|(k, v)| (k.as_str(), v.as_str()))
                        )
                        .unwrap(),
                    42
                );
            } else {
                assert_eq!(
                    writer
                        .add_channel_with_id(42, 0, "topic", "raw", &fields)
                        .unwrap(),
                    42
                );
                assert_eq!(writer.add_channel(0, "topic", "raw", &fields).unwrap(), 42);
            }
            writer.finish().unwrap();
            writer.into_inner().into_inner()
        };
        assert_eq!(recording(true), recording(false));
    }
}

#[cfg(test)]
mod charged_option_tests {
    use super::*;
    use crate::storage::{BudgetRef,OwnerKind};
    #[test]
    fn charged_text_clones_share_storage_and_preserve_headers() {
        let domain=BudgetRef::new(Default::default()).unwrap();
        let options=WriteOptions::new().memory_budget(domain.clone()).compression(None)
            .try_profile("profile\u{4e2d}").unwrap().try_library("library").unwrap();
        let before=domain.workload_detailed_statistics().allocation_count;
        let retained=domain.workload_statistics().current;
        assert!(retained>"profile\u{4e2d}library".len() as u64);
        let alias=options.clone();
        assert_eq!(domain.workload_detailed_statistics().allocation_count,before);
        drop(options);
        assert_eq!(domain.workload_statistics().current,retained);
        let mut bytes=std::io::Cursor::new(Vec::new());
        let writer=alias.create(&mut bytes).unwrap();drop(writer);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Operation as usize],0);
        let data=bytes.into_inner();
        let length=u64::from_le_bytes(data[9..17].try_into().unwrap()) as usize;
        let Record::Header(header)=crate::parse_record(op::HEADER,&data[17..17+length]).unwrap() else {panic!()};
        assert_eq!(header.profile,"profile\u{4e2d}");assert_eq!(header.library,"library");
    }
    #[test]
    fn charged_option_domain_change_is_rejected_before_output() {
        let original=BudgetRef::new(Default::default()).unwrap();
        let other=BudgetRef::new(Default::default()).unwrap();
        let options=WriteOptions::new().memory_budget(original.clone()).try_profile("profile").unwrap().memory_budget(other.clone());
        let mut output=std::io::Cursor::new(Vec::new());
        assert!(options.clone().create(&mut output).is_err());
        assert!(output.get_ref().is_empty());
        assert!(options.try_library("library").is_err());
        assert_eq!(original.workload_statistics().current,0);
        assert_eq!(other.workload_statistics().current,0);
    }
}
