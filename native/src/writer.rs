//! Writer state, audited rejections and synchronous write operations.
use super::{
    errors, Error, bytes, guard, map, number, request, respond, string, MessageHeader, Outcome,
    Response,
};
use super::io::{Callbacks, Output};
use super::summary::writer_summary_bytes;
use mcap::records;
use serde_json::Value;
use std::{borrow::Cow, fs::File, io::Write, ptr};

pub struct Writer {
    pub(super) inner: Option<mcap::Writer<Output>>,
    pub(super) completed_output: Option<Output>,
    pub(super) failed: bool,
    pub(super) recoverable_errors: u32,
    pub(super) attachment: Option<u64>,
    pub(super) native_summary: Option<std::sync::Arc<mcap::Summary>>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(w) = self.inner.take() {
            drop(w.into_inner());
        }
    }
}
pub(super) fn options(v: &Value, seekable: bool) -> Outcome<mcap::WriteOptions> {
    let compression = match string(v, "compression")? {
        "none" => None,
        "lz4" => Some(mcap::Compression::Lz4),
        "zstd" => Some(mcap::Compression::Zstd),
        _ => return Err("Unknown compression".into()),
    };
    let mut o = mcap::WriteOptions::new()
        .compression(compression)
        .chunk_size(v["chunkSize"].as_u64())
        .use_chunks(v["useChunks"].as_bool().unwrap_or(true))
        .profile(string(v, "profile")?)
        .library(v["library"].as_str().unwrap_or(mcap::LIBRARY_IDENTIFIER))
        .disable_seeking(v["disableSeeking"].as_bool().unwrap_or(!seekable));
    if !seekable && v["disableSeeking"].as_bool() == Some(false) {
        return Err("Non-seekable output requires DisableSeeking".into());
    }
    macro_rules! flag {
        ($key:literal,$method:ident) => {
            if let Some(x) = v[$key].as_bool() {
                o = o.$method(x);
            }
        };
    }
    flag!("emitSummaryRecords", emit_summary_records);
    flag!("emitSummaryOffsets", emit_summary_offsets);
    flag!("emitStatistics", emit_statistics);
    flag!("emitMessageIndexes", emit_message_indexes);
    flag!("emitChunkIndexes", emit_chunk_indexes);
    flag!("emitAttachmentIndexes", emit_attachment_indexes);
    flag!("emitMetadataIndexes", emit_metadata_indexes);
    flag!("repeatChannels", repeat_channels);
    flag!("repeatSchemas", repeat_schemas);
    flag!("calculateChunkCrcs", calculate_chunk_crcs);
    flag!("calculateDataSectionCrc", calculate_data_section_crc);
    flag!("calculateSummarySectionCrc", calculate_summary_section_crc);
    flag!("calculateAttachmentCrcs", calculate_attachment_crcs);
    if let Some(x) = v["compressionLevel"].as_u64() {
        o = o.compression_level(x.try_into()?);
    }
    if let Some(x) = v["compressionThreads"].as_u64() {
        o = o.compression_threads(x.try_into()?);
    }
    Ok(o)
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_open(
    p: *const u8,
    n: usize,
    callbacks: *const Callbacks,
    handle: *mut *mut Writer,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let recoverable_errors = match v["options"].get("recoverableErrors") {
            None => 31,
            Some(value) => value
                .as_u64()
                .filter(|n| n & !31 == 0)
                .ok_or("Invalid recovery flags")? as u32,
        };
        let output = if let Some(c) = callbacks.as_ref() {
            Output::Stream(*c)
        } else {
            Output::File(
                File::options()
                    .write(true)
                    .read(true)
                    .create_new(true)
                    .open(string(&v, "path")?)?,
            )
        };
        let seekable = callbacks.as_ref().map(|c| c.seekable != 0).unwrap_or(true);
        *handle = Box::into_raw(Box::new(Writer {
            inner: Some(options(&v["options"], seekable)?.create(output)?),
            completed_output: None,
            failed: false,
            recoverable_errors,
            attachment: None,
            native_summary: None,
        }));
        Ok(crate::protocol::status::SUCCESS)
    })
}
// Only audited, pre-mutation return sites may construct this marker.
#[derive(Debug)]
pub(super) struct SafeRejection(pub(super) mcap::McapError);
impl std::fmt::Display for SafeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for SafeRejection {}
pub(super) fn registration_result<T>(
    result: mcap::McapResult<T>,
    mask: u32,
    schema: bool,
    explicit: bool,
) -> Outcome<T> {
    result.map_err(|e| {
        let bit = match (&e, schema, explicit) {
            (mcap::McapError::InvalidSchemaId, true, true) => 1,
            (mcap::McapError::ConflictingSchemas(_), true, true) => 2,
            (mcap::McapError::UnknownSchema(..), false, _) => 4,
            (mcap::McapError::ConflictingChannels(_), false, true) => 8,
            _ => 0,
        };
        if mask & bit != 0 {
            Box::new(SafeRejection(e)) as Error
        } else {
            Box::new(e) as Error
        }
    })
}
pub(super) fn writer_guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    guard(out, |out| match f(out) {
        Err(e) if e.is::<SafeRejection>() => {
            let e = e.downcast::<SafeRejection>().unwrap();
            respond(out, errors::encode(&e.0), vec![], 0);
            Ok(crate::protocol::writer_status::SAFE_REJECTION)
        }
        result => result,
    })
}
pub(super) fn writer_result(handle: *mut Writer, status: i32) -> i32 {
    if status < crate::protocol::status::SUCCESS && status != crate::protocol::writer_status::SAFE_REJECTION {
        if let Some(w) = unsafe { handle.as_mut() } {
            w.failed = true;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_message(
    handle: *mut Writer,
    h: *const MessageHeader,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let s = writer_guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let h = h.as_ref().ok_or("Null header")?;
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write_to_known_channel(
                &records::MessageHeader {
                    channel_id: h.channel_id,
                    sequence: h.sequence,
                    log_time: h.log_time,
                    publish_time: h.publish_time,
                },
                bytes(p, n)?,
            )
            .map_err(|e| {
                if w.recoverable_errors & 16 != 0
                    && matches!(e, mcap::McapError::UnknownChannel(..))
                {
                    Box::new(SafeRejection(e)) as Error
                } else {
                    Box::new(e) as Error
                }
            })?;
        Ok(crate::protocol::status::SUCCESS)
    });
    writer_result(handle, s)
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_call(
    handle: *mut Writer,
    op: u32,
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    out: *mut Response,
) -> i32 {
    let status = writer_guard(out, |out| {
        let v = if n == 0 { Value::Null } else { request(p, n)? };
        writer_control(handle, op, &v, bytes(data, len)?, out)
    });
    writer_result(handle, status)
}
pub(super) unsafe fn writer_control(
    handle: *mut Writer,
    op: u32,
    v: &Value,
    payload: &[u8],
    out: &mut Response,
) -> Outcome<i32> {
    let holder = handle.as_mut().ok_or("Null writer")?;
    if holder.failed {
        return Err("Writer failed".into());
    }
    if op == crate::protocol::writer_operation::FLUSH_TO_DISK {
        holder
            .completed_output
            .as_mut()
            .ok_or("Complete must succeed first")?
            .sync_all()?;
        return Ok(crate::protocol::status::SUCCESS);
    }
    if op == crate::protocol::writer_operation::SUMMARY {
        if let Some(s) = &holder.native_summary {
            respond(out, writer_summary_bytes(s)?, vec![], 0);
            return Ok(crate::protocol::status::SUCCESS);
        }
        return Err("Complete must succeed first".into());
    }
    if holder.attachment.is_some() && op != crate::protocol::writer_operation::ATTACHMENT_BYTES && op != crate::protocol::writer_operation::FINISH_ATTACHMENT {
        return Err("Attachment is in progress".into());
    }
    let len = payload.len();
    let w = holder.inner.as_mut().ok_or("Writer completed")?;
    out.value = match op {
        crate::protocol::writer_operation::SCHEMA => {
            ({
                if let Some(id) = v["id"].as_u64() {
                    registration_result(
                        w.add_schema_with_id(
                            id.try_into()?,
                            string(v, "name")?,
                            string(v, "encoding")?,
                            payload,
                        ),
                        holder.recoverable_errors,
                        true,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_schema(string(v, "name")?, string(v, "encoding")?, payload),
                        holder.recoverable_errors,
                        true,
                        false,
                    )?
                }
            }) as u64
        }
        crate::protocol::writer_operation::CHANNEL => {
            ({
                if let Some(id) = v["id"].as_u64() {
                    registration_result(
                        w.add_channel_with_id(
                            id.try_into()?,
                            number(v, "schema_id")?.try_into()?,
                            string(v, "topic")?,
                            string(v, "encoding")?,
                            &map(&v["metadata"])?,
                        ),
                        holder.recoverable_errors,
                        false,
                        true,
                    )?
                } else {
                    registration_result(
                        w.add_channel(
                            number(v, "schema_id")?.try_into()?,
                            string(v, "topic")?,
                            string(v, "encoding")?,
                            &map(&v["metadata"])?,
                        ),
                        holder.recoverable_errors,
                        false,
                        false,
                    )?
                }
            }) as u64
        }
        crate::protocol::writer_operation::METADATA => {
            w.write_metadata(&records::Metadata {
                name: string(v, "name")?.into(),
                metadata: map(&v["metadata"])?,
            })?;
            0
        }
        crate::protocol::writer_operation::ATTACHMENT => {
            w.attach(&mcap::Attachment {
                name: string(v, "name")?.into(),
                media_type: string(v, "media_type")?.into(),
                log_time: number(v, "log_time")?,
                create_time: number(v, "create_time")?,
                data: Cow::Borrowed(payload),
            })?;
            0
        }
        crate::protocol::writer_operation::FLUSH => {
            w.flush()?;
            0
        }
        crate::protocol::writer_operation::COMPLETE => {
            let s = w.finish()?;
            // Release upstream's cached summary and declaration tables before any
            // binding-owned summary work. Keep the output for explicit persistence.
            holder.completed_output = Some(holder.inner.take().unwrap().into_inner());
            holder.native_summary = Some(std::sync::Arc::new(s));
            holder.completed_output.as_mut().unwrap().flush()?;
            0
        }
        crate::protocol::writer_operation::START_ATTACHMENT => {
            let size = number(v, "length")?;
            w.start_attachment(
                size,
                records::AttachmentHeader {
                    name: string(v, "name")?.into(),
                    media_type: string(v, "media_type")?.into(),
                    log_time: number(v, "log_time")?,
                    create_time: number(v, "create_time")?,
                },
            )?;
            holder.attachment = Some(size);
            0
        }
        crate::protocol::writer_operation::ATTACHMENT_BYTES => {
            let left = holder.attachment.as_mut().ok_or("No attachment")?;
            *left = left
                .checked_sub(len as u64)
                .ok_or("Attachment length exceeded")?;
            w.put_attachment_bytes(payload)?;
            0
        }
        crate::protocol::writer_operation::FINISH_ATTACHMENT => {
            if holder.attachment != Some(0) {
                return Err("Attachment length mismatch".into());
            }
            w.finish_attachment()?;
            holder.attachment = None;
            0
        }
        crate::protocol::writer_operation::PRIVATE_RECORD => {
            let opts = if v["includeInChunks"].as_bool().unwrap_or(false) {
                enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
            } else {
                enumset::EnumSet::new()
            };
            w.write_private_record(number(v, "opcode")?.try_into()?, payload, opts)?;
            0
        }
        _ => return Err("Unknown writer operation".into()),
    };
    Ok(crate::protocol::status::SUCCESS)
}


#[no_mangle]
pub unsafe extern "C" fn fm_writer_release(p: *mut Writer, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() { drop(Box::from_raw(p)); }
        Ok(crate::protocol::status::SUCCESS)
    })
}

