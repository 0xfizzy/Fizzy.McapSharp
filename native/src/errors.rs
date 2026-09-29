use super::*;
pub(super) fn encode(e: &(dyn std::error::Error + 'static)) -> Vec<u8> {
    if let Some(e) = e.downcast_ref::<memory::Limit>() {
        return serde_json::to_vec(&json!({"kind":"Binding","message":e.to_string(),"details":{"resource":e.resource,"limit":e.limit,"requested":e.requested}})).unwrap();
    }
    use mcap::McapError::*;
    let (kind, details) = if let Some(e) = e.downcast_ref::<mcap::McapError>() {
        match e {
            AttachmentNotInProgress => ("AttachmentNotInProgress", json!({})),
            AttachmentTooLarge {
                excess,
                attachment_length,
            } => (
                "AttachmentTooLarge",
                json!({"excess": excess, "attachment_length": attachment_length}),
            ),
            AttachmentIncomplete { current, expected } => (
                "AttachmentIncomplete",
                json!({"current": current, "expected": expected}),
            ),
            BadMagic => ("BadMagic", json!({})),
            BadFooter => ("BadFooter", json!({})),
            BadAttachmentCrc { saved, calculated } => (
                "BadAttachmentCrc",
                json!({"saved": saved, "calculated": calculated}),
            ),
            BadChunkCrc { saved, calculated } => (
                "BadChunkCrc",
                json!({"saved": saved, "calculated": calculated}),
            ),
            BadDataCrc { saved, calculated } => (
                "BadDataCrc",
                json!({"saved": saved, "calculated": calculated}),
            ),
            BadSummaryCrc { saved, calculated } => (
                "BadSummaryCrc",
                json!({"saved": saved, "calculated": calculated}),
            ),
            BadIndex => ("BadIndex", json!({})),
            BadAttachmentLength { header, available } => (
                "BadAttachmentLength",
                json!({"header": header, "available": available}),
            ),
            BadChunkLength { header, available } => (
                "BadChunkLength",
                json!({"header": header, "available": available}),
            ),
            BadSchemaLength { header, available } => (
                "BadSchemaLength",
                json!({"header": header, "available": available}),
            ),
            PrivateRecordOpcodeIsReserved { opcode } => {
                ("PrivateRecordOpcodeIsReserved", json!({"opcode": opcode}))
            }
            ConflictingChannels(name) => ("ConflictingChannels", json!({"name": name})),
            ConflictingSchemas(name) => ("ConflictingSchemas", json!({"name": name})),
            Parse(source) => ("Parse", json!({"source": source.to_string()})),
            Io(source) => ("Io", json!({"source": source.to_string()})),
            InvalidSchemaId => ("InvalidSchemaId", json!({})),
            UnexpectedEof => ("UnexpectedEof", json!({})),
            UnexpectedEoc => ("UnexpectedEoc", json!({})),
            UnknownChannel(sequence, channel_id) => (
                "UnknownChannel",
                json!({"sequence": sequence, "channel_id": channel_id}),
            ),
            UnknownSchema(name, schema_id) => (
                "UnknownSchema",
                json!({"name": name, "schema_id": schema_id}),
            ),
            UnsupportedCompression(compression) => (
                "UnsupportedCompression",
                json!({"compression": compression}),
            ),
            DecompressionError(description) => {
                ("DecompressionError", json!({"description": description}))
            }
            ChunkBufferTooLarge(length) => ("ChunkBufferTooLarge", json!({"length": length})),
            RecordTooLarge { opcode, len } => {
                ("RecordTooLarge", json!({"opcode": opcode, "len": len}))
            }
            ChunkTooLarge(length) => ("ChunkTooLarge", json!({"length": length})),
            BadChunkStartOffset(offset) => ("BadChunkStartOffset", json!({"offset": offset})),
            TooManyChannels => ("TooManyChannels", json!({})),
            TooManySchemas => ("TooManySchemas", json!({})),
            UnexpectedChunkDataInserted => ("UnexpectedChunkDataInserted", json!({})),
            AttemptedWriteAfterFailure => ("AttemptedWriteAfterFailure", json!({})),
            BytesAfterEndMagic => ("BytesAfterEndMagic", json!({})),
        }
    } else {
        ("Binding", Value::Null)
    };
    serde_json::to_vec(&json!({"kind":kind,"message":e.to_string(),"details":details}))
        .unwrap_or_else(|_| b"Native error serialization failed".to_vec())
}
