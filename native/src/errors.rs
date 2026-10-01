#[path = "native_error.rs"]
mod native_error;
pub use native_error::NativeError;
use super::*;
#[cfg(test)]
pub(super) fn legacy_encode(e: &(dyn std::error::Error + 'static)) -> Vec<u8> {
    if let Some(e) = e.downcast_ref::<memory::Limit>() {
        return serde_json::to_vec(&json!({"kind":"Binding","message":e.to_string(),"details":{"resource":e.resource,"limit":e.limit,"domainLimit":e.domain_limit,"current":e.current,"phase":"resource-check","failureKind":"permanent","requested":e.requested}})).unwrap();
    }
    let io = e.downcast_ref::<std::io::Error>().or_else(|| match e.downcast_ref::<mcap::McapError>() { Some(mcap::McapError::Io(io)) => Some(io), _ => None });
    if let Some(limit) = io.and_then(|io|io.get_ref()).and_then(|e|e.downcast_ref::<mcap::storage::StorageLimit>()) {
        return serde_json::to_vec(&json!({"kind":"Binding","message":limit.to_string(),"details":{"resource":limit.resource,"limit":limit.limit,"domainLimit":limit.domain_limit,"requested":limit.requested,"current":limit.current,"phase":limit.phase}})).unwrap();
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
            StaticParseError {position,description} => ("Parse",json!({"source":description,"position":position})),
            Io(source) => ("Io", json!({"source": source.to_string()})),
            StaticIoError(source) => ("Io", json!({"source": source})),
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
            StaticDecompressionError(description) => ("DecompressionError", json!({"description": description})),
            Lz4Error(name) => ("Io", json!({"source": format!("LZ4 error: {name}")})),
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
            Storage(failure) => ("Binding", json!({"resource":failure.details.resource,
                "limit":failure.details.limit,"domainLimit":failure.details.domain_limit,"requested":failure.details.requested,"current":failure.details.current,
                "phase":failure.details.phase,"failureKind":failure.kind.as_str(),"terminal":failure.terminal})),
        }
    } else {
        ("Binding", Value::Null)
    };
    serde_json::to_vec(&json!({"kind":kind,"message":e.to_string(),"details":details}))
        .unwrap_or_else(|_| b"Native error serialization failed".to_vec())
}

use std::fmt::{self, Write as _};
pub const ERROR_CAPACITY: usize = 4096;
#[repr(C)]
pub struct FixedError { pub length: usize, pub bytes: [u8; ERROR_CAPACITY] }
impl Default for FixedError {
    fn default() -> Self { Self { length: 0, bytes: [0; ERROR_CAPACITY] } }
}
impl FixedError {
    pub fn as_bytes(&self) -> &[u8] { &self.bytes[..self.length] }
    pub fn panic(&mut self) {
        self.length = 0;
        let _ = self.write_str("Native MCAP panic; operation failed");
    }
}
impl fmt::Write for FixedError {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length.checked_add(text.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        target.copy_from_slice(text.as_bytes()); self.length = end; Ok(())
    }
}
#[derive(Debug)]
struct Text<const N: usize> { bytes: [u8; N], length: usize, truncated: bool }
impl<const N: usize> Text<N> {
    fn display(value: &dyn fmt::Display) -> Self {
        let mut text = Self { bytes: [0; N], length: 0, truncated: false };
        if write!(&mut text, "{value}").is_err() { text.truncated = true; }
        text
    }
    fn as_str(&self) -> &str { std::str::from_utf8(&self.bytes[..self.length]).unwrap() }
}
impl<const N: usize> fmt::Display for Text<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())?;
        if self.truncated { f.write_str("... [truncated]")?; }
        Ok(())
    }
}
impl<const N: usize> std::error::Error for Text<N> {}
impl<const N: usize> fmt::Write for Text<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut count = text.len().min(N - self.length);
        while !text.is_char_boundary(count) { count -= 1; }
        self.bytes[self.length..self.length+count].copy_from_slice(&text.as_bytes()[..count]);
        self.length += count;
        if count < text.len() { self.truncated = true; Err(fmt::Error) } else { Ok(()) }
    }
}
fn quoted(out: &mut FixedError, text: &str, truncated: bool) -> fmt::Result {
    out.write_char('"')?;
    for ch in text.chars() {
        match ch {
            '"' => out.write_str("\\\"")?, '\\' => out.write_str("\\\\")?,
            ch if ch < '\u{20}' => write!(out, "\\u{:04x}", ch as u32)?,
            ch => out.write_char(ch)?,
        }
    }
    if truncated { out.write_str("... [truncated]")?; }
    out.write_char('"')
}
#[derive(Clone, Copy)]
enum Detail<'a> { Number(u64), Signed(i64), Bool(bool), Text(&'a dyn fmt::Display) }
#[derive(Default)]
struct Details<'a> { values: [Option<(&'static str, Detail<'a>)>; 8], count: usize, null: bool }
impl<'a> Details<'a> {
    fn null() -> Self { Self { null: true, ..Self::default() } }
    fn push(&mut self, key: &'static str, value: Detail<'a>) { self.values[self.count] = Some((key,value)); self.count += 1; }
}
macro_rules! details {
    ($(($key:expr, $value:expr)),* $(,)?) => {{
        let mut details = Details::default(); $(details.push($key, $value);)* details
    }};
}
fn render(out: &mut FixedError, kind: &str, error: &dyn fmt::Display, details: Details<'_>) -> fmt::Result {
    let message = Text::<256>::display(error);
    render_message(out, kind, &message, details)
}
fn render_message(out: &mut FixedError, kind: &str, message: &Text<256>, details: Details<'_>) -> fmt::Result {
    out.write_str("{\"kind\":")?; quoted(out, kind, false)?;
    out.write_str(",\"message\":")?; quoted(out, message.as_str(), message.truncated)?;
    out.write_str(",\"details\":")?;
    if details.null { out.write_str("null")?; } else {
        out.write_char('{')?;
        let mut truncated = message.truncated;
        for (i, &(key, value)) in details.values[..details.count].iter().flatten().enumerate() {
            if i != 0 { out.write_char(',')?; }
            quoted(out, key, false)?; out.write_char(':')?;
            match value {
                Detail::Number(n) => write!(out, "{n}")?,
                Detail::Signed(n) => write!(out, "{n}")?,
                Detail::Bool(value) => write!(out, "{value}")?,
                Detail::Text(value) => { let text = Text::<128>::display(value); truncated |= text.truncated;
                    quoted(out, text.as_str(), text.truncated)?; }
            }
        }
        if truncated { if details.count != 0 { out.write_char(',')?; } out.write_str("\"truncated\":true")?; }
        out.write_char('}')?;
    }
    out.write_char('}')
}
pub(super) fn encode_into(out: &mut FixedError, e: &(dyn std::error::Error + 'static)) {
    out.length = 0;
    if encode_fixed(out, e).is_err() {
        // A bounded fallback also works after system allocation failure.
        out.length = 0;
        let _ = out.write_str("Native error exceeded fixed response capacity");
    }
}
fn encode_fixed(out: &mut FixedError, e: &(dyn std::error::Error + 'static)) -> fmt::Result {
    if let Some(message) = e.downcast_ref::<Text<256>>() {
        return render_message(out, "Binding", message, Details::null());
    }
    if let Some(e) = e.downcast_ref::<memory::Limit>() {
        return render(out, "Binding", e, details![("resource", Detail::Text(&e.resource)),
            ("limit", Detail::Number(e.limit)), ("requested", Detail::Number(e.requested)),
            ("domainLimit", Detail::Number(e.domain_limit)), ("current", Detail::Number(e.current)),
            ("phase", Detail::Text(&"resource-check")), ("failureKind", Detail::Text(&"permanent"))]);
    }
    let io = e.downcast_ref::<std::io::Error>().or_else(|| match e.downcast_ref::<mcap::McapError>() { Some(mcap::McapError::Io(io)) => Some(io), _ => None });
    if let Some(code) = io.and_then(std::io::Error::raw_os_error) {
        // std::io::Error's OS-message lookup allocates on Windows. Keep the
        // exact OS code without asking the OS to allocate a localized message.
        struct OsCode(i32);
        impl fmt::Display for OsCode {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "Operating system error {}", self.0)
            }
        }
        let message = OsCode(code);
        let kind = if e.is::<mcap::McapError>() { "Io" } else { "Binding" };
        return render(out, kind, &message, details![("source", Detail::Text(&message)),
            ("osCode", Detail::Signed(code as i64))]);
    }
    if let Some(limit) = e.downcast_ref::<mcap::storage::StorageLimit>()
        .or_else(|| match e.downcast_ref::<mcap::storage::BootstrapError>() {
            Some(mcap::storage::BootstrapError::Limit(limit)) => Some(limit), _ => None,
        })
        .or_else(|| io.and_then(|io|io.get_ref()).and_then(|e|e.downcast_ref::<mcap::storage::StorageLimit>())) {
        return render(out, "Binding", limit, details![("resource", Detail::Text(&limit.resource)),
            ("limit", Detail::Number(limit.limit as u64)), ("domainLimit", Detail::Number(limit.domain_limit as u64)), ("requested", Detail::Number(limit.requested as u64)),
            ("current", Detail::Number(limit.current as u64)), ("phase", Detail::Text(&limit.phase))]);
    }
    use mcap::McapError::*;
    let (kind, details) = if let Some(e) = e.downcast_ref::<mcap::McapError>() {
        match e {
            AttachmentNotInProgress => ("AttachmentNotInProgress", Details::default()),
            AttachmentTooLarge {
                excess,
                attachment_length,
            } => (
                "AttachmentTooLarge",
                details![("excess", Detail::Number(*excess as u64)), ("attachment_length", Detail::Number(*attachment_length as u64))],
            ),
            AttachmentIncomplete { current, expected } => (
                "AttachmentIncomplete",
                details![("current", Detail::Number(*current as u64)), ("expected", Detail::Number(*expected as u64))],
            ),
            BadMagic => ("BadMagic", Details::default()),
            BadFooter => ("BadFooter", Details::default()),
            BadAttachmentCrc { saved, calculated } => (
                "BadAttachmentCrc",
                details![("saved", Detail::Number(*saved as u64)), ("calculated", Detail::Number(*calculated as u64))],
            ),
            BadChunkCrc { saved, calculated } => (
                "BadChunkCrc",
                details![("saved", Detail::Number(*saved as u64)), ("calculated", Detail::Number(*calculated as u64))],
            ),
            BadDataCrc { saved, calculated } => (
                "BadDataCrc",
                details![("saved", Detail::Number(*saved as u64)), ("calculated", Detail::Number(*calculated as u64))],
            ),
            BadSummaryCrc { saved, calculated } => (
                "BadSummaryCrc",
                details![("saved", Detail::Number(*saved as u64)), ("calculated", Detail::Number(*calculated as u64))],
            ),
            BadIndex => ("BadIndex", Details::default()),
            BadAttachmentLength { header, available } => (
                "BadAttachmentLength",
                details![("header", Detail::Number(*header as u64)), ("available", Detail::Number(*available as u64))],
            ),
            BadChunkLength { header, available } => (
                "BadChunkLength",
                details![("header", Detail::Number(*header as u64)), ("available", Detail::Number(*available as u64))],
            ),
            BadSchemaLength { header, available } => (
                "BadSchemaLength",
                details![("header", Detail::Number(*header as u64)), ("available", Detail::Number(*available as u64))],
            ),
            PrivateRecordOpcodeIsReserved { opcode } => {
                ("PrivateRecordOpcodeIsReserved", details![("opcode", Detail::Number(*opcode as u64))])
            }
            ConflictingChannels(name) => ("ConflictingChannels", details![("name", Detail::Text(name))]),
            ConflictingSchemas(name) => ("ConflictingSchemas", details![("name", Detail::Text(name))]),
            Parse(source) => ("Parse", details![("source", Detail::Text(source.root_cause()))]),
            StaticParseError {position,description} => ("Parse",details![("source",Detail::Text(description)),("position",Detail::Number(*position))]),
            Io(source) => ("Io", details![("source", Detail::Text(source))]),
            StaticIoError(source) => ("Io", details![("source", Detail::Text(source))]),
            InvalidSchemaId => ("InvalidSchemaId", Details::default()),
            UnexpectedEof => ("UnexpectedEof", Details::default()),
            UnexpectedEoc => ("UnexpectedEoc", Details::default()),
            UnknownChannel(sequence, channel_id) => (
                "UnknownChannel",
                details![("sequence", Detail::Number(*sequence as u64)), ("channel_id", Detail::Number(*channel_id as u64))],
            ),
            UnknownSchema(name, schema_id) => (
                "UnknownSchema",
                details![("name", Detail::Text(name)), ("schema_id", Detail::Number(*schema_id as u64))],
            ),
            UnsupportedCompression(compression) => (
                "UnsupportedCompression",
                details![("compression", Detail::Text(compression))],
            ),
            DecompressionError(description) => {
                ("DecompressionError", details![("description", Detail::Text(description))])
            }
            StaticDecompressionError(description) => ("DecompressionError", details![("description", Detail::Text(description))]),
            Lz4Error(name) => {
                let source = format_args!("LZ4 error: {name}");
                return render(out, "Io", e, details![("source", Detail::Text(&source))]);
            }
            ChunkBufferTooLarge(length) => ("ChunkBufferTooLarge", details![("length", Detail::Number(*length as u64))]),
            RecordTooLarge { opcode, len } => {
                ("RecordTooLarge", details![("opcode", Detail::Number(*opcode as u64)), ("len", Detail::Number(*len as u64))])
            }
            ChunkTooLarge(length) => ("ChunkTooLarge", details![("length", Detail::Number(*length as u64))]),
            BadChunkStartOffset(offset) => ("BadChunkStartOffset", details![("offset", Detail::Number(*offset as u64))]),
            TooManyChannels => ("TooManyChannels", Details::default()),
            TooManySchemas => ("TooManySchemas", Details::default()),
            UnexpectedChunkDataInserted => ("UnexpectedChunkDataInserted", Details::default()),
            AttemptedWriteAfterFailure => ("AttemptedWriteAfterFailure", Details::default()),
            BytesAfterEndMagic => ("BytesAfterEndMagic", Details::default()),
            Storage(failure) => ("Binding", details![
                ("resource", Detail::Text(&failure.details.resource)), ("limit", Detail::Number(failure.details.limit as u64)), ("domainLimit", Detail::Number(failure.details.domain_limit as u64)),
                ("requested", Detail::Number(failure.details.requested as u64)), ("current", Detail::Number(failure.details.current as u64)),
                ("phase", Detail::Text(&failure.details.phase)), ("failureKind", Detail::Text(&failure.kind)),
                ("terminal", Detail::Bool(failure.terminal))]),
        }
    } else {
        ("Binding", Details::null())
    };
    render(out, kind, e, details)
}
#[cfg(test)]
pub(super) fn encode(e: &(dyn std::error::Error + 'static)) -> Vec<u8> {
    let mut out = FixedError::default(); encode_into(&mut out, e); out.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_json_matches_previous_fields_for_every_official_variant() {
        use mcap::McapError::*;
        let cases = [AttachmentNotInProgress, AttachmentTooLarge {excess:u64::MAX,attachment_length:3},
            AttachmentIncomplete {current:1,expected:2}, BadMagic, BadFooter,
            BadAttachmentCrc {saved:1,calculated:2}, BadChunkCrc {saved:1,calculated:2},
            BadDataCrc {saved:1,calculated:2}, BadSummaryCrc {saved:1,calculated:2}, BadIndex,
            BadAttachmentLength {header:1,available:2}, BadChunkLength {header:1,available:2},
            BadSchemaLength {header:1,available:2}, PrivateRecordOpcodeIsReserved {opcode:4},
            ConflictingChannels("channel".into()), ConflictingSchemas("schema".into()),
            Io(std::io::Error::other("stream failure")), InvalidSchemaId, UnexpectedEof, UnexpectedEoc,
            UnknownChannel(3,4), UnknownSchema("schema".into(),5), UnsupportedCompression("unknown".into()),
            DecompressionError("invalid frame".into()), StaticDecompressionError("invalid frame"),
            Lz4Error("ERROR_frameType_unknown"),
            Storage(mcap::storage::StorageFailure {details:mcap::storage::StorageLimit {
                resource:"CodecDecoder",limit:1024,domain_limit:1024,requested:4096,current:1024,phase:"reservation"},
                kind:mcap::storage::StorageFailureKind::PermanentLimit,terminal:true}),
            ChunkBufferTooLarge(u64::MAX),
            RecordTooLarge {opcode:3,len:u64::MAX}, ChunkTooLarge(u64::MAX), BadChunkStartOffset(u64::MAX),
            TooManyChannels, TooManySchemas, UnexpectedChunkDataInserted, AttemptedWriteAfterFailure, BytesAfterEndMagic];
        for error in cases {
            let old: Value = serde_json::from_slice(&legacy_encode(&error)).unwrap();
            let new: Value = serde_json::from_slice(&encode(&error)).unwrap();
            assert_eq!(new,old);
        }
        // Obtain the official parser's concrete Parse error without fabricating its layout.
        let error = mcap::parse_record(records::op::MESSAGE, &[]).unwrap_err();
        let actual: Value = serde_json::from_slice(&encode(&error)).unwrap();
        assert_eq!(actual["kind"], "Parse");
        if let Parse(source) = error {
            assert_eq!(actual["details"]["source"], source.root_cause().to_string());
        } else { panic!("Expected parser error"); }
    }
}
