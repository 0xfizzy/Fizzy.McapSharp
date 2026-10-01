//! Stream shared summary storage without constructing JSON trees or full arrays.
use super::*;
use mcap::{segmented::SharedSegmentedVec, shared_statistics::SharedStatistics,
    shared_chunk_index::SharedChunkIndex, shared_attachment_index::SharedAttachmentIndex,
    shared_metadata_index::SharedMetadataIndex};
#[derive(Clone)]
pub(super) struct Observed {
    pub schemas: SharedSegmentedVec<u16>,
    pub channels: SharedSegmentedVec<u16>,
}
impl Observed {
    pub fn new(domain: &mcap::storage::BudgetRef) -> Self {
        Self {schemas:SharedSegmentedVec::new(domain.clone()),channels:SharedSegmentedVec::new(domain.clone())}
    }
}
pub(super) struct View<'a> {
    pub statistics: Option<&'a SharedStatistics>,
    pub chunks: &'a SharedSegmentedVec<SharedChunkIndex>,
    pub attachments: &'a SharedSegmentedVec<SharedAttachmentIndex>,
    pub metadata: &'a SharedSegmentedVec<SharedMetadataIndex>,
}
fn ids(writer: &mut dyn Write, ids: impl Iterator<Item=u16>) -> Outcome<()> {
    writer.write_all(b"[")?;
    for (i,id) in ids.enumerate() {if i!=0 {writer.write_all(b",")?;}serde_json::to_writer(&mut *writer,&id)?;}
    writer.write_all(b"]")?;Ok(())
}
fn counts<'a>(writer: &mut dyn Write, values: impl Iterator<Item=(u16,&'a u64)>) -> Outcome<()> {
    writer.write_all(b"{")?;
    for (i,(id,value)) in values.enumerate() {
        if i!=0 {writer.write_all(b",")?;}
        write!(writer,"\"{}\":",id)?;serde_json::to_writer(&mut *writer,value)?;
    }
    writer.write_all(b"}")?;Ok(())
}
fn statistics(writer: &mut dyn Write, s: &SharedStatistics) -> Outcome<()> {
    writer.write_all(b"{\"messageCount\":")?;serde_json::to_writer(&mut *writer,&s.message_count)?;
    writer.write_all(b",\"schemaCount\":")?;serde_json::to_writer(&mut *writer,&s.schema_count)?;
    writer.write_all(b",\"channelCount\":")?;serde_json::to_writer(&mut *writer,&s.channel_count)?;
    writer.write_all(b",\"attachmentCount\":")?;serde_json::to_writer(&mut *writer,&s.attachment_count)?;
    writer.write_all(b",\"metadataCount\":")?;serde_json::to_writer(&mut *writer,&s.metadata_count)?;
    writer.write_all(b",\"chunkCount\":")?;serde_json::to_writer(&mut *writer,&s.chunk_count)?;
    writer.write_all(b",\"messageStartTime\":")?;serde_json::to_writer(&mut *writer,&s.message_start_time)?;
    writer.write_all(b",\"messageEndTime\":")?;serde_json::to_writer(&mut *writer,&s.message_end_time)?;
    writer.write_all(b",\"channelMessageCounts\":")?;counts(writer,s.channel_message_counts.iter())?;
    writer.write_all(b"}")?;Ok(())
}
fn chunk(writer: &mut dyn Write, s: &SharedChunkIndex) -> Outcome<()> {
    writer.write_all(b"{\"messageStartTime\":")?;serde_json::to_writer(&mut *writer,&s.message_start_time)?;
    writer.write_all(b",\"messageEndTime\":")?;serde_json::to_writer(&mut *writer,&s.message_end_time)?;
    writer.write_all(b",\"chunkStartOffset\":")?;serde_json::to_writer(&mut *writer,&s.chunk_start_offset)?;
    writer.write_all(b",\"chunkLength\":")?;serde_json::to_writer(&mut *writer,&s.chunk_length)?;
    writer.write_all(b",\"messageIndexLength\":")?;serde_json::to_writer(&mut *writer,&s.message_index_length)?;
    writer.write_all(b",\"compression\":")?;serde_json::to_writer(&mut *writer,&s.compression)?;
    writer.write_all(b",\"compressedSize\":")?;serde_json::to_writer(&mut *writer,&s.compressed_size)?;
    writer.write_all(b",\"uncompressedSize\":")?;serde_json::to_writer(&mut *writer,&s.uncompressed_size)?;
    writer.write_all(b",\"messageIndexOffsets\":")?;counts(writer,s.message_index_offsets.iter())?;
    writer.write_all(b"}")?;Ok(())
}
fn attachment(writer: &mut dyn Write, s: &SharedAttachmentIndex) -> Outcome<()> {
    writer.write_all(b"{\"offset\":")?;serde_json::to_writer(&mut *writer,&s.offset)?;
    writer.write_all(b",\"length\":")?;serde_json::to_writer(&mut *writer,&s.length)?;
    writer.write_all(b",\"logTime\":")?;serde_json::to_writer(&mut *writer,&s.log_time)?;
    writer.write_all(b",\"createTime\":")?;serde_json::to_writer(&mut *writer,&s.create_time)?;
    writer.write_all(b",\"dataSize\":")?;serde_json::to_writer(&mut *writer,&s.data_size)?;
    writer.write_all(b",\"name\":")?;serde_json::to_writer(&mut *writer,&s.name)?;
    writer.write_all(b",\"mediaType\":")?;serde_json::to_writer(&mut *writer,&s.media_type)?;
    writer.write_all(b"}")?;Ok(())
}
fn metadata(writer: &mut dyn Write, s: &SharedMetadataIndex) -> Outcome<()> {
    writer.write_all(b"{\"offset\":")?;serde_json::to_writer(&mut *writer,&s.offset)?;
    writer.write_all(b",\"length\":")?;serde_json::to_writer(&mut *writer,&s.length)?;
    writer.write_all(b",\"name\":")?;serde_json::to_writer(&mut *writer,&s.name)?;
    writer.write_all(b"}")?;Ok(())
}
impl View<'_> {
    fn write(&self, writer: &mut dyn Write) -> Outcome<()> {
        writer.write_all(b"{\"statistics\":")?;
        if let Some(value)=self.statistics {statistics(writer,value)?;} else {writer.write_all(b"null")?;}
        writer.write_all(b",\"chunkIndexes\":[")?;
        for (i,value) in self.chunks.iter().enumerate() {if i!=0 {writer.write_all(b",")?;}chunk(writer,value)?;}
        writer.write_all(b"]")?;
        writer.write_all(b",\"attachmentIndexes\":[")?;
        for (i,value) in self.attachments.iter().enumerate() {if i!=0 {writer.write_all(b",")?;}attachment(writer,value)?;}
        writer.write_all(b"]")?;
        writer.write_all(b",\"metadataIndexes\":[")?;
        for (i,value) in self.metadata.iter().enumerate() {if i!=0 {writer.write_all(b",")?;}metadata(writer,value)?;}
        writer.write_all(b"]")?;
        Ok(())
    }
    pub fn respond(&self, out: &mut Response, domain: &mcap::storage::BudgetRef, observed: &Observed) -> Outcome<()> {
        response::write(out,domain,|writer| {
            self.write(writer)?;
            writer.write_all(b",\"schemaIds\":")?;ids(writer,observed.schemas.iter().copied())?;
            writer.write_all(b",\"channelIds\":")?;ids(writer,observed.channels.iter().copied())?;
            writer.write_all(b"}")?;Ok(())
        },&[],0)
    }
}
pub(super) fn respond(out: &mut Response, domain: &mcap::storage::BudgetRef, summary: &mcap::Summary) -> Outcome<()> {
    let view=View {statistics:summary.stats.as_ref(),chunks:&summary.chunk_indexes,
        attachments:&summary.attachment_indexes,metadata:&summary.metadata_indexes};
    response::write(out,domain,|writer| {
        view.write(writer)?;
        writer.write_all(b",\"schemaIds\":")?;ids(writer,summary.schemas.keys())?;
        writer.write_all(b",\"channelIds\":")?;ids(writer,summary.channels.keys())?;
        writer.write_all(b"}")?;Ok(())
    },&[],0)
}
