//! Owned summary serialization shared by reader, writer and snapshot responses.
use super::Outcome;
use mcap::records;
use serde_json::{json, Value};

pub(super) fn chunk_json(c: &records::ChunkIndex) -> Value {
    json!({"messageStartTime":c.message_start_time,"messageEndTime":c.message_end_time,"chunkStartOffset":c.chunk_start_offset,"chunkLength":c.chunk_length,"messageIndexOffsets":c.message_index_offsets,"messageIndexLength":c.message_index_length,"compression":c.compression,"compressedSize":c.compressed_size,"uncompressedSize":c.uncompressed_size})
}
pub(super) fn stats_json(s: &records::Statistics) -> Value {
    json!({"messageCount":s.message_count,"schemaCount":s.schema_count,"channelCount":s.channel_count,"attachmentCount":s.attachment_count,"metadataCount":s.metadata_count,"chunkCount":s.chunk_count,"messageStartTime":s.message_start_time,"messageEndTime":s.message_end_time,"channelMessageCounts":s.channel_message_counts})
}
pub(super) fn attachment_index_json(a: &records::AttachmentIndex) -> Value {
    json!({"offset":a.offset,"length":a.length,"logTime":a.log_time,"createTime":a.create_time,"dataSize":a.data_size,"name":a.name,"mediaType":a.media_type})
}
pub(super) fn metadata_index_json(a: &records::MetadataIndex) -> Value {
    json!({"offset":a.offset,"length":a.length,"name":a.name})
}
pub(super) fn summary_json(s: &mcap::Summary) -> Value {
    json!({"statistics":s.stats.as_ref().map(stats_json),"chunkIndexes":s.chunk_indexes.iter().map(chunk_json).collect::<Vec<_>>(),"attachmentIndexes":s.attachment_indexes.iter().map(attachment_index_json).collect::<Vec<_>>(),"metadataIndexes":s.metadata_indexes.iter().map(metadata_index_json).collect::<Vec<_>>(),"schemaIds":s.schemas.keys().collect::<Vec<_>>(),"channelIds":s.channels.keys().collect::<Vec<_>>()})
}
// Cold writer response: only the final UTF-8 buffer and the current record's
// JSON value coexist. Never retain a file-wide JSON tree alongside the summary.
pub(super) fn writer_summary_bytes(s: &mcap::Summary) -> Outcome<Vec<u8>> {
    fn array(out: &mut Vec<u8>, values: impl Iterator<Item = Value>) -> Outcome<()> {
        out.push(b'[');
        for (i, value) in values.enumerate() {
            if i != 0 {
                out.push(b',');
            }
            serde_json::to_writer(&mut *out, &value)?;
        }
        out.push(b']');
        Ok(())
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"{\"statistics\":");
    serde_json::to_writer(&mut out, &s.stats.as_ref().map(stats_json))?;
    out.extend_from_slice(b",\"chunkIndexes\":");
    array(&mut out, s.chunk_indexes.iter().map(chunk_json))?;
    out.extend_from_slice(b",\"attachmentIndexes\":");
    array(
        &mut out,
        s.attachment_indexes.iter().map(attachment_index_json),
    )?;
    out.extend_from_slice(b",\"metadataIndexes\":");
    array(&mut out, s.metadata_indexes.iter().map(metadata_index_json))?;
    out.extend_from_slice(b",\"schemaIds\":");
    array(&mut out, s.schemas.keys().map(|id| json!(id)))?;
    out.extend_from_slice(b",\"channelIds\":");
    array(&mut out, s.channels.keys().map(|id| json!(id)))?;
    out.push(b'}');
    Ok(out)
}
pub(super) fn empty_summary() -> Value {
    json!({"statistics":null,"chunkIndexes":[],"attachmentIndexes":[],"metadataIndexes":[],"schemaIds":[],"channelIds":[]})
}

