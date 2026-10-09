use super::{memory, OperationRestoreError};
use serde_json::{json, Value};
// Error paths may receive attacker-controlled names. Bound formatting itself,
// not just the serialized output, so an oversized Display need not be copied.
fn bounded(value: &dyn std::fmt::Display, limit: usize) -> (String, bool) {
    use std::fmt::Write;
    struct Text {
        value: String,
        limit: usize,
        truncated: bool,
    }
    impl std::fmt::Write for Text {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let remaining = self.limit - self.value.len();
            if value.len() <= remaining {
                self.value.push_str(value);
                return Ok(());
            }
            let mut end = remaining;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.value.push_str(&value[..end]);
            self.truncated = true;
            Err(std::fmt::Error)
        }
    }
    let mut text = Text {
        value: String::with_capacity(limit),
        limit,
        truncated: false,
    };
    let _ = write!(text, "{value}");
    if text.truncated {
        const SUFFIX: &str = "[truncated]";
        let mut end = text.value.len().min(limit - SUFFIX.len());
        while !text.value.is_char_boundary(end) {
            end -= 1;
        }
        text.value.truncate(end);
        text.value.push_str(SUFFIX);
    }
    (text.value, text.truncated)
}

pub(super) fn encode(e: &(dyn std::error::Error + 'static)) -> Vec<u8> {
    if let Some(e) = e.downcast_ref::<OperationRestoreError>() {
        let operation: Value = serde_json::from_slice(&encode(e.operation.as_ref())).unwrap();
        let cleanup: Value = serde_json::from_slice(&encode(&e.cleanup)).unwrap();
        return serde_json::to_vec(&json!({"kind":"Binding", "message":"Operation and source-position restoration failed", "details":{"code":"operationRestore","operation":operation,"cleanup":cleanup}})).unwrap();
    }
    let mut truncated = false;
    let mut text = |value: &dyn std::fmt::Display| {
        let (value, shortened) = bounded(value, 128);
        truncated |= shortened;
        value
    };
    use mcap::McapError::*;
    let (kind, mut details) = if let Some(e) = e.downcast_ref::<mcap::McapError>() {
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
            ConflictingChannels(name) => ("ConflictingChannels", json!({"name": text(name)})),
            ConflictingSchemas(name) => ("ConflictingSchemas", json!({"name": text(name)})),
            Parse(source) => (
                "Parse",
                if let binrw::Error::Io(io) = source.root_cause() {
                    if let Some(code) = io.raw_os_error() {
                        json!({"source": "Operating system I/O error", "osCode": code})
                    } else {
                        json!({"source": text(source.root_cause())})
                    }
                } else {
                    json!({"source": text(source.root_cause())})
                },
            ),
            Io(source) => (
                "Io",
                if let Some(code) = source.raw_os_error() {
                    json!({"source": "Operating system I/O error", "osCode": code})
                } else {
                    json!({"source": text(source)})
                },
            ),
            InvalidSchemaId => ("InvalidSchemaId", json!({})),
            UnexpectedEof => ("UnexpectedEof", json!({})),
            UnexpectedEoc => ("UnexpectedEoc", json!({})),
            UnknownChannel(sequence, channel_id) => (
                "UnknownChannel",
                json!({"sequence": sequence, "channel_id": channel_id}),
            ),
            UnknownSchema(name, schema_id) => (
                "UnknownSchema",
                json!({"name": text(name), "schema_id": schema_id}),
            ),
            UnsupportedCompression(compression) => (
                "UnsupportedCompression",
                json!({"compression": text(compression)}),
            ),
            DecompressionError(description) => (
                "DecompressionError",
                json!({"description": text(description)}),
            ),
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
    } else if let Some(e) = e.downcast_ref::<memory::Limit>() {
        (
            "Binding",
            json!({"resource": text(&e.resource), "limit": e.limit, "requested": e.requested}),
        )
    } else {
        ("Binding", Value::Null)
    };
    let os_error = if let Some(mcap::McapError::Io(source)) = e.downcast_ref::<mcap::McapError>() {
        source.raw_os_error()
    } else if let Some(mcap::McapError::Parse(source)) = e.downcast_ref::<mcap::McapError>() {
        if let binrw::Error::Io(io) = source.root_cause() {
            io.raw_os_error()
        } else {
            None
        }
    } else if let Some(source) = e.downcast_ref::<std::io::Error>() {
        source.raw_os_error()
    } else {
        None
    };
    let (message, shortened) = if let Some(code) = os_error {
        details = json!({"source": "Operating system I/O error", "osCode": code});
        ("Operating system I/O error".to_owned(), false)
    } else if let Some(mcap::McapError::Parse(source)) = e.downcast_ref::<mcap::McapError>() {
        bounded(source.root_cause(), 256)
    } else {
        bounded(&e, 256)
    };
    if truncated || shortened {
        if details.is_null() {
            details = json!({});
        }
        details["truncated"] = json!(true);
    }
    serde_json::to_vec(&json!({"kind":kind,"message":message,"details":details}))
        .unwrap_or_else(|_| b"Native error serialization failed".to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded(error: &(dyn std::error::Error + 'static)) -> Value {
        serde_json::from_slice(&encode(error)).unwrap()
    }

    #[test]
    fn truncation_bounds_utf8_and_preserves_numeric_details() {
        let value = encoded(&mcap::McapError::UnknownSchema("界".repeat(1000), u16::MAX));
        let message = value["message"].as_str().unwrap();
        let name = value["details"]["name"].as_str().unwrap();
        assert!(message.len() <= 256 && message.ends_with("[truncated]"));
        assert!(name.len() <= 128 && name.ends_with("[truncated]"));
        assert_eq!(value["details"]["schema_id"], u16::MAX);
        assert_eq!(value["details"]["truncated"], true);
        for length in [127, 128, 129, 255, 256, 257] {
            let error = std::io::Error::other("a".repeat(length));
            let value = encoded(&error);
            assert!(value["message"].as_str().unwrap().len() <= 256);
            assert_eq!(
                value["details"]["truncated"].as_bool(),
                (length > 256).then_some(true)
            );
        }
    }

    #[test]
    fn os_errors_use_fixed_text_and_exact_code() {
        for error in [
            mcap::McapError::Io(std::io::Error::from_raw_os_error(2)),
            mcap::McapError::Parse(binrw::Error::Io(std::io::Error::from_raw_os_error(2))),
        ] {
            let value = encoded(&error);
            assert_eq!(value["message"], "Operating system I/O error");
            assert_eq!(value["details"]["osCode"], 2);
            assert_eq!(value["details"]["source"], "Operating system I/O error");
        }
    }

    #[test]
    fn parse_errors_omit_backtrace_decoration() {
        use binrw::error::{Backtrace, BacktraceFrame};
        let root = binrw::Error::AssertFail {
            pos: 7,
            message: "root parse reason".into(),
        };
        let expected = root.to_string();
        let source = binrw::Error::Backtrace(Backtrace::new(
            root,
            vec![BacktraceFrame::Message("decorated parser context".into())],
        ));
        let value = encoded(&mcap::McapError::Parse(source));
        assert_eq!(value["message"], expected);
        assert_eq!(value["details"]["source"], expected);
        assert!(!value.to_string().contains("decorated parser context"));
    }

    #[test]
    fn simultaneous_failures_keep_separately_bounded_causes() {
        let error = OperationRestoreError {
            operation: std::io::Error::other("o".repeat(1000)).into(),
            cleanup: std::io::Error::other("restoration failed"),
        };
        let value = encoded(&error);
        assert!(value["details"]["operation"]["message"]
            .as_str()
            .unwrap()
            .ends_with("[truncated]"));
        assert_eq!(value["details"]["cleanup"]["message"], "restoration failed");
    }
}
