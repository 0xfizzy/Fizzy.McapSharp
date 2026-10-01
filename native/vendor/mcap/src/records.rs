//! Raw records parsed from an MCAP file
//!
//! See <https://mcap.dev/spec>
//!
//! You probably want to use higher-level interfaces, like
//! [`Message`](crate::Message), [`Channel`](crate::Channel), and [`Schema`](crate::Schema),
//! read from iterators like [`MessageStream`](crate::MessageStream).

use std::{borrow::Cow, collections::BTreeMap};

use binrw::*;

use crate::McapResult;

/// Opcodes for MCAP file records.
///
/// "Records are identified by a single-byte opcode.
/// Record opcodes in the range 0x01-0x7F are reserved for future MCAP format usage.
/// 0x80-0xFF are reserved for application extensions and user proposals."
pub mod op {
    pub const HEADER: u8 = 0x01;
    pub const FOOTER: u8 = 0x02;
    pub const SCHEMA: u8 = 0x03;
    pub const CHANNEL: u8 = 0x04;
    pub const MESSAGE: u8 = 0x05;
    pub const CHUNK: u8 = 0x06;
    pub const MESSAGE_INDEX: u8 = 0x07;
    pub const CHUNK_INDEX: u8 = 0x08;
    pub const ATTACHMENT: u8 = 0x09;
    pub const ATTACHMENT_INDEX: u8 = 0x0A;
    pub const STATISTICS: u8 = 0x0B;
    pub const METADATA: u8 = 0x0C;
    pub const METADATA_INDEX: u8 = 0x0D;
    pub const SUMMARY_OFFSET: u8 = 0x0E;
    pub const DATA_END: u8 = 0x0F;
}

/// A raw record from an MCAP file.
///
/// For records with large slices of binary data (schemas, messages, chunks...),
/// we use a [`CoW`](std::borrow::Cow) that can either borrow directly from the mapped file,
/// or hold its own buffer if it was decompressed from a chunk.
#[derive(Debug)]
pub enum Record<'a> {
    Header(Header),
    Footer(Footer),
    Schema {
        header: SchemaHeader,
        data: Cow<'a, [u8]>,
    },
    Channel(Channel),
    Message {
        header: MessageHeader,
        data: Cow<'a, [u8]>,
    },
    Chunk {
        header: ChunkHeader,
        data: Cow<'a, [u8]>,
    },
    MessageIndex(MessageIndex),
    ChunkIndex(ChunkIndex),
    Attachment {
        header: AttachmentHeader,
        data: Cow<'a, [u8]>,
        crc: u32,
    },
    AttachmentIndex(AttachmentIndex),
    Statistics(Statistics),
    Metadata(Metadata),
    MetadataIndex(MetadataIndex),
    SummaryOffset(SummaryOffset),
    DataEnd(DataEnd),
    /// A record of unknown type
    Unknown {
        opcode: u8,
        data: Cow<'a, [u8]>,
    },
}

impl Record<'_> {
    pub fn opcode(&self) -> u8 {
        match &self {
            Record::Header(_) => op::HEADER,
            Record::Footer(_) => op::FOOTER,
            Record::Schema { .. } => op::SCHEMA,
            Record::Channel(_) => op::CHANNEL,
            Record::Message { .. } => op::MESSAGE,
            Record::Chunk { .. } => op::CHUNK,
            Record::MessageIndex(_) => op::MESSAGE_INDEX,
            Record::ChunkIndex(_) => op::CHUNK_INDEX,
            Record::Attachment { .. } => op::ATTACHMENT,
            Record::AttachmentIndex(_) => op::ATTACHMENT_INDEX,
            Record::Statistics(_) => op::STATISTICS,
            Record::Metadata(_) => op::METADATA,
            Record::MetadataIndex(_) => op::METADATA_INDEX,
            Record::SummaryOffset(_) => op::SUMMARY_OFFSET,
            Record::DataEnd(_) => op::DATA_END,
            Record::Unknown { opcode, .. } => *opcode,
        }
    }

    /// Moves this value into a fully-owned variant with no borrows. This should be free for
    /// already-owned values.
    pub fn into_owned(self) -> Record<'static> {
        match self {
            Record::Header(header) => Record::Header(header),
            Record::Footer(footer) => Record::Footer(footer),
            Record::Schema { header, data } => Record::Schema {
                header,
                data: Cow::Owned(data.into_owned()),
            },
            Record::Channel(channel) => Record::Channel(channel),
            Record::Message { header, data } => Record::Message {
                header,
                data: Cow::Owned(data.into_owned()),
            },
            Record::Chunk { header, data } => Record::Chunk {
                header,
                data: Cow::Owned(data.into_owned()),
            },
            Record::MessageIndex(index) => Record::MessageIndex(index),
            Record::ChunkIndex(index) => Record::ChunkIndex(index),
            Record::Attachment { header, data, crc } => Record::Attachment {
                header,
                data: Cow::Owned(data.into_owned()),
                crc,
            },
            Record::AttachmentIndex(index) => Record::AttachmentIndex(index),
            Record::Statistics(statistics) => Record::Statistics(statistics),
            Record::Metadata(metadata) => Record::Metadata(metadata),
            Record::MetadataIndex(index) => Record::MetadataIndex(index),
            Record::SummaryOffset(offset) => Record::SummaryOffset(offset),
            Record::DataEnd(end) => Record::DataEnd(end),
            Record::Unknown { opcode, data } => Record::Unknown {
                opcode,
                data: Cow::Owned(data.into_owned()),
            },
        }
    }
}

#[binrw]
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct McapString {
    #[br(temp)]
    #[bw(calc = inner.len() as u32)]
    pub len: u32,

    #[br(count = len, try_map = String::from_utf8)]
    #[bw(map = |s| s.as_bytes())]
    pub inner: String,
}

/// Avoids taking a copy to turn a String to an McapString for serialization
#[binrw::writer(writer, endian)]
fn write_string(s: &String) -> BinResult<()> {
    write_str(s, writer, endian)
}

fn write_str<W: std::io::Write + std::io::Seek>(s: &str, writer: &mut W, endian: Endian) -> BinResult<()> {
    (s.len() as u32).write_options(writer, endian, ())?;
    (s.as_bytes()).write_options(writer, endian, ())?;
    Ok(())
}

#[binrw::parser(reader, endian)]
fn parse_vec<T: BinRead<Args<'static> = ()>>() -> BinResult<Vec<T>> {
    let mut parsed = Vec::new();

    // Length of the map in BYTES, not records.
    let byte_len: u32 = BinRead::read_options(reader, endian, ())?;
    let pos = reader.stream_position()?;

    while (reader.stream_position()? - pos) < byte_len as u64 {
        parsed.push(T::read_options(reader, endian, ())?);
    }

    Ok(parsed)
}

#[expect(clippy::ptr_arg)]
#[binrw::writer(writer, endian)]
fn write_vec<T: BinWrite<Args<'static> = ()>>(v: &Vec<T>) -> BinResult<()> {
    use std::io::SeekFrom;
    let start = writer.stream_position()?;
    (!0u32).write_options(writer, endian, ())?; // Revisit...
    for e in v.iter() {
        e.write_options(writer, endian, ())?;
    }
    let end = writer.stream_position()?;
    let data_len = end - start - 4;
    writer.seek(SeekFrom::Start(start))?;
    (data_len as u32).write_options(writer, endian, ())?;
    assert_eq!(writer.seek(SeekFrom::End(0))?, end);
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct Header {
    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub profile: String,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub library: String,
}
#[derive(BinWrite)]
pub(crate) struct HeaderRef<'a> {
    #[bw(write_with = write_string_ref)]
    pub(crate) profile: &'a str,
    #[bw(write_with = write_string_ref)]
    pub(crate) library: &'a str,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, BinRead, BinWrite)]
pub struct Footer {
    pub summary_start: u64,
    pub summary_offset_start: u64,
    pub summary_crc: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct SchemaHeader {
    pub id: u16,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub name: String,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub encoding: String,
}

#[binrw::parser(reader, endian)]
fn parse_string_map() -> BinResult<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();

    // Length of the map in BYTES, not records.
    let byte_len: u32 = BinRead::read_options(reader, endian, ())?;
    let pos = reader.stream_position()?;

    while (reader.stream_position()? - pos) < byte_len as u64 {
        let k = McapString::read_options(reader, endian, ())?;
        let v = McapString::read_options(reader, endian, ())?;
        if let Some(_prev) = parsed.insert(k.inner, v.inner) {
            return Err(binrw::Error::Custom {
                pos,
                err: Box::new("Duplicate keys in map"),
            });
        }
    }

    Ok(parsed)
}

#[binrw::writer(writer, endian)]
fn write_string_map(s: &BTreeMap<String, String>) -> BinResult<()> {
    write_string_pairs(s.iter().map(|(k,v)| (k.as_str(),v.as_str())), writer, endian)
}

fn write_string_pairs<'a, W, I>(pairs: I, writer: &mut W, endian: Endian) -> BinResult<()>
where W: std::io::Write + std::io::Seek, I: Clone + Iterator<Item=(&'a str, &'a str)> {
    // Ugh: figure out total number of bytes to write:
    let mut byte_len = 0;
    for (k, v) in pairs.clone() {
        byte_len += 8; // Four bytes each for lengths of key and value
        byte_len += k.len();
        byte_len += v.len();
    }

    (byte_len as u32).write_options(writer, endian, ())?;
    let pos = writer.stream_position()?;

    for (k, v) in pairs {
        write_str(k, writer, endian)?;
        write_str(v, writer, endian)?;
    }
    assert_eq!(writer.stream_position()?, pos + byte_len as u64);
    Ok(())
}

/// Serialization-only metadata view; cloned iterators must yield identical pairs.
pub(crate) struct MetadataRef<'a, I> {
    pub(crate) name: &'a str,
    pub(crate) pairs: I,
}
impl<'a, I: Clone + Iterator<Item=(&'a str, &'a str)>> BinWrite for MetadataRef<'a, I> {
    type Args<'b> = ();
    fn write_options<W: std::io::Write + std::io::Seek>(&self, writer: &mut W, endian: Endian, _: ()) -> BinResult<()> {
        write_str(self.name, writer, endian)?;
        write_string_pairs(self.pairs.clone(), writer, endian)
    }
}

#[binrw::writer(writer, endian)]
fn write_int_map<K: BinWrite<Args<'static> = ()>, V: BinWrite<Args<'static> = ()>>(
    s: &BTreeMap<K, V>,
) -> BinResult<()> {
    // Ugh: figure out total number of bytes to write:
    let mut byte_len = 0;
    for _ in s.values() {
        // Hack: We're assuming serialized size of the value is its in-memory size.
        // For ints of all flavors, this should be true.
        byte_len += core::mem::size_of::<K>();
        byte_len += core::mem::size_of::<V>();
    }

    (byte_len as u32).write_options(writer, endian, ())?;
    let pos = writer.stream_position()?;

    for (k, v) in s {
        k.write_options(writer, endian, ())?;
        v.write_options(writer, endian, ())?;
    }
    assert_eq!(writer.stream_position()?, pos + byte_len as u64);
    Ok(())
}

#[binrw::parser(reader, endian)]
fn parse_int_map<K: BinRead<Args<'static> = ()> + std::cmp::Ord, V: BinRead<Args<'static> = ()>>(
) -> BinResult<BTreeMap<K, V>> {
    let mut parsed = BTreeMap::new();

    // Length of the map in BYTES, not records.
    let byte_len: u32 = BinRead::read_options(reader, endian, ())?;
    let pos = reader.stream_position()?;

    while (reader.stream_position()? - pos) < byte_len as u64 {
        let k = K::read_options(reader, endian, ())?;
        let v = V::read_options(reader, endian, ())?;
        if let Some(_prev) = parsed.insert(k, v) {
            return Err(binrw::Error::Custom {
                pos,
                err: Box::new("Duplicate keys in map"),
            });
        }
    }

    Ok(parsed)
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct Channel {
    pub id: u16,
    pub schema_id: u16,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub topic: String,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub message_encoding: String,

    #[br(parse_with = parse_string_map)]
    #[bw(write_with = write_string_map)]
    pub metadata: BTreeMap<String, String>,
}

#[binrw::writer(writer, endian)]
fn write_string_ref(s: &&str) -> BinResult<()> {
    write_str(s, writer, endian)
}
#[cfg(test)]
#[binrw::writer(writer, endian)]
fn write_string_map_ref(s: &&BTreeMap<String, String>) -> BinResult<()> {
    write_string_map(s, writer, endian, ())
}

/// Serialization-only views reuse the owned records' field writers without cloning data.
#[cfg(test)]
#[derive(BinWrite)]
pub(crate) struct ChannelRef<'a> {
    pub(crate) id: u16,
    pub(crate) schema_id: u16,
    #[bw(write_with = write_string_ref)]
    pub(crate) topic: &'a str,
    #[bw(write_with = write_string_ref)]
    pub(crate) message_encoding: &'a str,
    #[bw(write_with = write_string_map_ref)]
    pub(crate) metadata: &'a BTreeMap<String, String>,
}
pub(crate) struct ChannelPairsRef<'a, I> {
    pub(crate) id: u16,
    pub(crate) schema_id: u16,
    pub(crate) topic: &'a str,
    pub(crate) message_encoding: &'a str,
    pub(crate) metadata: I,
}
impl<'a, I: Clone + Iterator<Item=(&'a str, &'a str)>> BinWrite for ChannelPairsRef<'a, I> {
    type Args<'b> = ();
    fn write_options<W: std::io::Write + std::io::Seek>(&self, writer: &mut W, endian: Endian, _: ()) -> BinResult<()> {
        self.id.write_options(writer, endian, ())?;
        self.schema_id.write_options(writer, endian, ())?;
        write_str(self.topic, writer, endian)?;
        write_str(self.message_encoding, writer, endian)?;
        write_string_pairs(self.metadata.clone(), writer, endian)
    }
}
#[derive(BinWrite)]
pub(crate) struct SchemaHeaderRef<'a> {
    pub(crate) id: u16,
    #[bw(write_with = write_string_ref)]
    pub(crate) name: &'a str,
    #[bw(write_with = write_string_ref)]
    pub(crate) encoding: &'a str,
}

/// Borrowed summary bodies. Serialization reuses the official field writers and
/// never clones declarations, strings, or nested index tables.
pub enum SummaryRecordRef<'a> {
    Channel(&'a crate::shared_declarations::SharedChannel),
    Schema(&'a crate::Schema<'static>),
    Statistics(&'a crate::shared_statistics::SharedStatistics),
    ChunkIndex(&'a crate::shared_chunk_index::SharedChunkIndex),
    AttachmentIndex(&'a AttachmentIndex),
    MetadataIndex(&'a MetadataIndex),
}
impl SummaryRecordRef<'_> {
    pub fn opcode(&self) -> u8 {
        match self {
            Self::Channel(_) => op::CHANNEL,
            Self::Schema(_) => op::SCHEMA,
            Self::Statistics(_) => op::STATISTICS,
            Self::ChunkIndex(_) => op::CHUNK_INDEX,
            Self::AttachmentIndex(_) => op::ATTACHMENT_INDEX,
            Self::MetadataIndex(_) => op::METADATA_INDEX,
        }
    }
    pub fn write_body<W: std::io::Write + std::io::Seek>(&self, out: &mut W) -> BinResult<()> {
        match self {
            Self::Channel(c) => ChannelPairsRef {
                id: c.id, schema_id: c.schema_id,
                topic: &c.topic, message_encoding: &c.message_encoding, metadata: c.metadata.iter().map(|(k,v)|(k.as_str(),v.as_str())),
            }.write_le(out),
            Self::Schema(s) => {
                let length = u32::try_from(s.data.len()).map_err(|_| binrw::Error::Io(
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "Schema data exceeds wire length")))?;
                SchemaHeaderRef { id: s.id, name: &s.name, encoding: &s.encoding }.write_le(out)?;
                out.write_all(&length.to_le_bytes())?;
                out.write_all(&s.data)?;
                Ok(())
            }
            Self::Statistics(v) => v.write_le(out),
            Self::ChunkIndex(v) => v.write_le(out),
            Self::AttachmentIndex(v) => v.write_le(out),
            Self::MetadataIndex(v) => v.write_le(out),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct MessageHeader {
    pub channel_id: u16,
    pub sequence: u32,

    pub log_time: u64,

    pub publish_time: u64,
}

impl MessageHeader {
    pub(crate) fn serialized_len(&self) -> u64 {
        2  // channel ID
        + 4  // sequence
        + 8  // log time
        + 8 // publish time
    }
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct ChunkHeader {
    pub message_start_time: u64,

    pub message_end_time: u64,

    pub uncompressed_size: u64,

    pub uncompressed_crc: u32,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub compression: String,

    pub compressed_size: u64,
}
#[derive(BinWrite)]
pub(crate) struct ChunkHeaderRef<'a> {
    pub(crate) message_start_time: u64,
    pub(crate) message_end_time: u64,
    pub(crate) uncompressed_size: u64,
    pub(crate) uncompressed_crc: u32,
    #[bw(write_with = write_string_ref)]
    pub(crate) compression: &'a str,
    pub(crate) compressed_size: u64,
}

/// Borrowed representation of the same wire header, used while parser input is pinned.
pub struct BorrowedChunkHeader<'a> {
    pub uncompressed_size: u64,
    pub uncompressed_crc: u32,
    pub compression: &'a str,
    pub compressed_size: u64,
}
impl<'a> BorrowedChunkHeader<'a> {
    pub(crate) fn read(bytes: &'a [u8]) -> BinResult<Self> {
        let eof = || binrw::Error::Io(std::io::ErrorKind::UnexpectedEof.into());
        let prefix = bytes.get(..32).ok_or_else(eof)?;
        let compression_len = u32::from_le_bytes(prefix[28..32].try_into().unwrap()) as usize;
        let end = 32usize.checked_add(compression_len).ok_or_else(eof)?;
        let compression = std::str::from_utf8(bytes.get(32..end).ok_or_else(eof)?)
            .map_err(|_| binrw::Error::Io(std::io::ErrorKind::InvalidData.into()))?;
        let tail = bytes
            .get(end..end.checked_add(8).ok_or_else(eof)?)
            .ok_or_else(eof)?;
        Ok(Self {
            uncompressed_size: u64::from_le_bytes(prefix[16..24].try_into().unwrap()),
            uncompressed_crc: u32::from_le_bytes(prefix[24..28].try_into().unwrap()),
            compression,
            compressed_size: u64::from_le_bytes(tail.try_into().unwrap()),
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, BinRead, BinWrite)]
pub struct MessageIndexEntry {
    pub log_time: u64,

    pub offset: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct MessageIndex {
    pub channel_id: u16,

    #[br(parse_with = parse_vec)]
    #[bw(write_with = write_vec)]
    pub records: Vec<MessageIndexEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct ChunkIndex {
    pub message_start_time: u64,

    pub message_end_time: u64,

    pub chunk_start_offset: u64,

    pub chunk_length: u64,

    #[br(parse_with = parse_int_map)]
    #[bw(write_with = write_int_map)]
    pub message_index_offsets: BTreeMap<u16, u64>,

    pub message_index_length: u64,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub compression: String,

    pub compressed_size: u64,

    pub uncompressed_size: u64,
}

impl ChunkIndex {
    /// Returns the offset in the file to the start of compressed chunk data.
    /// This can be useful for retrieving just the compressed content of a chunk given its index.
    /// Returns [`McapError::BadChunkStartOffset`] if the resulting offset would be greater than [`u64::MAX`].
    pub fn compressed_data_offset(&self) -> McapResult<u64> {
        crate::shared_chunk_index::compressed_data_offset(self.chunk_start_offset, self.compression.len())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct AttachmentHeader {
    pub log_time: u64,

    pub create_time: u64,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub name: String,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub media_type: String,
}

#[derive(BinWrite)]
pub(crate) struct AttachmentHeaderRef<'a> {
    pub(crate) log_time: u64,
    pub(crate) create_time: u64,
    #[bw(write_with = write_string_ref)]
    pub(crate) name: &'a str,
    #[bw(write_with = write_string_ref)]
    pub(crate) media_type: &'a str,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct AttachmentIndex {
    pub offset: u64,

    pub length: u64,

    pub log_time: u64,

    pub create_time: u64,

    pub data_size: u64,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub name: String,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub media_type: String,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct Statistics {
    pub message_count: u64,
    pub schema_count: u16,
    pub channel_count: u32,
    pub attachment_count: u32,
    pub metadata_count: u32,
    pub chunk_count: u32,

    pub message_start_time: u64,

    pub message_end_time: u64,

    #[br(parse_with = parse_int_map)]
    #[bw(write_with = write_int_map)]
    pub channel_message_counts: BTreeMap<u16, u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct Metadata {
    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub name: String,

    #[br(parse_with = parse_string_map)]
    #[bw(write_with = write_string_map)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Eq, PartialEq, BinRead, BinWrite)]
pub struct MetadataIndex {
    pub offset: u64,

    pub length: u64,

    #[br(map = |s: McapString| s.inner )]
    #[bw(write_with = write_string)]
    pub name: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, BinRead, BinWrite)]
pub struct SummaryOffset {
    pub group_opcode: u8,
    pub group_start: u64,
    pub group_length: u64,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, BinRead, BinWrite)]
pub struct DataEnd {
    pub data_section_crc: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn string_parse() {
        let ms: McapString = Cursor::new(b"\x04\0\0\0abcd").read_le().unwrap();
        assert_eq!(
            ms,
            McapString {
                inner: String::from("abcd")
            }
        );

        assert!(Cursor::new(b"\x05\0\0\0abcd")
            .read_le::<McapString>()
            .is_err());

        let mut written = Vec::new();
        Cursor::new(&mut written)
            .write_le(&McapString {
                inner: String::from("hullo"),
            })
            .unwrap();
        assert_eq!(&written, b"\x05\0\0\0hullo");
    }

    #[test]
    fn header_parse() {
        let expected = b"\x04\0\0\0abcd\x03\0\0\x00123";

        let h: Header = Cursor::new(expected).read_le().unwrap();
        assert_eq!(h.profile, "abcd");
        assert_eq!(h.library, "123");

        let mut written = Vec::new();
        Cursor::new(&mut written).write_le(&h).unwrap();
        assert_eq!(written, expected);
    }

    #[test]
    fn test_message_header_len() {
        let header = MessageHeader {
            sequence: 1,
            log_time: 2,
            channel_id: 3,
            publish_time: 4,
        };

        let len = header.serialized_len();

        let mut buf = vec![];
        Cursor::new(&mut buf).write_le(&header).unwrap();

        assert_eq!(len as usize, buf.len());
    }
}

#[cfg(test)]
mod borrowed_chunk_tests {
    use super::*;
    #[test]
    fn borrowed_header_matches_official_owned_header_and_rejects_truncation() {
        for compression in ["", "lz4", "zstd", "unknown", "编码"] {
            let header = ChunkHeader {
                message_start_time: 17,
                message_end_time: 91,
                uncompressed_size: 12345,
                uncompressed_crc: 123,
                compression: compression.into(),
                compressed_size: 6789,
            };
            let mut bytes = std::io::Cursor::new(Vec::new());
            bytes.write_le(&header).unwrap();
            let bytes = bytes.into_inner();
            let borrowed = BorrowedChunkHeader::read(&bytes).unwrap();
            assert_eq!(borrowed.compression, header.compression);
            assert_eq!(borrowed.uncompressed_size, header.uncompressed_size);
            assert_eq!(borrowed.uncompressed_crc, header.uncompressed_crc);
            assert_eq!(borrowed.compressed_size, header.compressed_size);
            for end in 0..bytes.len() {
                assert!(BorrowedChunkHeader::read(&bytes[..end]).is_err());
                assert!(std::io::Cursor::new(&bytes[..end])
                    .read_le::<ChunkHeader>()
                    .is_err());
            }
        }
    }
    #[test]
    fn invalid_utf8_is_rejected_by_both_header_readers() {
        let mut bytes = vec![0u8; 41];
        bytes[28] = 1;
        bytes[32] = 0xff;
        assert!(BorrowedChunkHeader::read(&bytes).is_err());
        assert!(std::io::Cursor::new(&bytes)
            .read_le::<ChunkHeader>()
            .is_err());
    }
}

#[cfg(test)]
mod declaration_view_tests {
    use super::*;
    #[test]
    fn borrowed_declarations_match_owned_serialization() {
        for text in ["", "/é/通道"] {
            let channel = Channel {
                id: 65535,
                schema_id: 42,
                topic: text.into(),
                message_encoding: "raw".into(),
                metadata: BTreeMap::from([("z".into(), "末".into()), ("a".into(), text.into())]),
            };
            let view = ChannelRef {
                id: channel.id,
                schema_id: channel.schema_id,
                topic: &channel.topic,
                message_encoding: &channel.message_encoding,
                metadata: &channel.metadata,
            };
            let mut owned = std::io::Cursor::new(Vec::new());
            let mut borrowed = std::io::Cursor::new(Vec::new());
            owned.write_le(&channel).unwrap();
            borrowed.write_le(&view).unwrap();
            assert_eq!(owned.into_inner(), borrowed.into_inner());
            let header = SchemaHeader {
                id: 42,
                name: text.into(),
                encoding: "jsonschema".into(),
            };
            let view = SchemaHeaderRef {
                id: header.id,
                name: &header.name,
                encoding: &header.encoding,
            };
            let mut owned = std::io::Cursor::new(Vec::new());
            let mut borrowed = std::io::Cursor::new(Vec::new());
            owned.write_le(&header).unwrap();
            borrowed.write_le(&view).unwrap();
            assert_eq!(owned.into_inner(), borrowed.into_inner());
        }
    }
}

#[cfg(test)]
mod summary_view_tests {
    use super::*;
    #[test]
    fn summary_declaration_bodies_roundtrip_through_official_parser() {
        for text in ["", "通道/é"] {
            let schema = std::sync::Arc::new(crate::Schema {
                id: 42, name: text.into(), encoding: "raw".into(),
                data: std::borrow::Cow::Owned(vec![0, 1, 255]),
            });
            let channel = crate::Channel {
                id: 65535, topic: text.into(), message_encoding: "raw".into(),
                schema: Some(schema.clone()),
                metadata: BTreeMap::from([("key".into(), text.into())]),
            };
            let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
            let shared_schema = crate::shared_declarations::SharedSchema::new(schema.id,&schema.name,&schema.encoding,&schema.data,&domain,crate::storage::OwnerKind::Parser).unwrap();
            let shared_channel = crate::shared_declarations::SharedChannel::new(channel.id,&channel.topic,&channel.message_encoding,Some(shared_schema.clone()),channel.metadata.iter().map(|(k,v)|(k.as_str(),v.as_str())),&domain,crate::storage::OwnerKind::Parser).unwrap();
            for view in [SummaryRecordRef::Schema(&shared_schema), SummaryRecordRef::Channel(&shared_channel)] {
                let mut body = std::io::Cursor::new(Vec::new());
                view.write_body(&mut body).unwrap();
                let bytes = body.into_inner();
                match crate::parse_record(view.opcode(), &bytes).unwrap() {
                    Record::Schema { header, data } => {
                        assert_eq!(header.id, schema.id);
                        assert_eq!(header.name, schema.name);
                        assert_eq!(header.encoding, schema.encoding);
                        assert_eq!(data, schema.data);
                    }
                    Record::Channel(v) => {
                        assert_eq!(v.id, channel.id);
                        assert_eq!(v.schema_id, schema.id);
                        assert_eq!(v.topic, channel.topic);
                        assert_eq!(v.message_encoding, channel.message_encoding);
                        assert_eq!(v.metadata, channel.metadata);
                    }
                    _ => panic!("unexpected declaration"),
                }
                // Writing into a fixed caller buffer must produce the identical body.
                let mut fixed = vec![0; bytes.len()];
                view.write_body(&mut std::io::Cursor::new(fixed.as_mut_slice())).unwrap();
                assert_eq!(fixed, bytes);
            }
        }
    }
}

#[cfg(test)]
mod metadata_view_tests {
    use super::*;
    #[test]
    fn metadata_pairs_match_owned_wire_and_parse() {
        for name in ["", "é/元数据"] {
            let metadata=Metadata {name:name.into(),metadata:BTreeMap::from([("a".into(),"é".into()),("z".into(),"末".into())])};
            let mut expected=std::io::Cursor::new(Vec::new());
            metadata.write_le(&mut expected).unwrap();
            let mut actual=std::io::Cursor::new(Vec::new());
            MetadataRef {name,pairs:metadata.metadata.iter().map(|(k,v)|(k.as_str(),v.as_str()))}.write_le(&mut actual).unwrap();
            assert_eq!(actual.get_ref(),expected.get_ref());
            let Record::Metadata(parsed)=crate::parse_record(op::METADATA,actual.get_ref()).unwrap() else {panic!()};
            assert_eq!(parsed,metadata);
        }
    }
}

#[cfg(test)]
mod writer_header_view_tests {
    #[test]
    fn borrowed_attachment_header_matches_official_fields() {
        use binrw::BinWrite;
        for (name, media_type) in [("", ""), ("名字🌍", "数据/类型")] {
            let owned = super::AttachmentHeader { log_time: 1, create_time: u64::MAX, name: name.into(), media_type: media_type.into() };
            let borrowed = super::AttachmentHeaderRef { log_time: 1, create_time: u64::MAX, name, media_type };
            let mut a = std::io::Cursor::new(Vec::new()); let mut b = std::io::Cursor::new(Vec::new());
            owned.write_le(&mut a).unwrap(); borrowed.write_le(&mut b).unwrap();
            assert_eq!(a.into_inner(), b.into_inner());
        }
    }
    use super::*;
    #[test]
    fn borrowed_file_and_chunk_headers_match_owned_wire() {
        for (profile, library) in [("", ""), ("profile/通道", "library\0build")] {
            let owned = Header { profile: profile.into(), library: library.into() };
            let view = HeaderRef { profile, library };
            let mut expected = std::io::Cursor::new(Vec::new());
            let mut actual = std::io::Cursor::new(Vec::new());
            owned.write_le(&mut expected).unwrap();
            view.write_le(&mut actual).unwrap();
            assert_eq!(actual.into_inner(), expected.into_inner());
        }
        for compression in ["", "lz4", "zstd"] {
            let owned = ChunkHeader { message_start_time: 1, message_end_time: u64::MAX, uncompressed_size: 3, uncompressed_crc: 42, compression: compression.into(), compressed_size: 5 };
            let view = ChunkHeaderRef { message_start_time: 1, message_end_time: u64::MAX, uncompressed_size: 3, uncompressed_crc: 42, compression, compressed_size: 5 };
            let mut expected = std::io::Cursor::new(Vec::new());
            let mut actual = std::io::Cursor::new(Vec::new());
            owned.write_le(&mut expected).unwrap();
            view.write_le(&mut actual).unwrap();
            assert_eq!(actual.into_inner(), expected.into_inner());
        }
    }
}
