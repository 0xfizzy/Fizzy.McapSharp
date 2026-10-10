use super::writer::{Writer, writer_control, writer_guard, writer_result};
use super::{bytes, guard, map, number, request, string, MessageHeader, Response};
use serde_json::Value;
use std::borrow::Cow;
use std::ptr;
use std::sync::Arc;

pub struct PreparedChannel(Arc<mcap::Channel<'static>>);

pub struct PreparedOperation {
    op: u32,
    args: Value,
    data: Vec<u8>,
}

#[no_mangle]
pub unsafe extern "C" fn fm_operation_prepare(
    op: u32,
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    handle: *mut *mut PreparedOperation,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        if !matches!(op, crate::protocol::writer_operation::SCHEMA | crate::protocol::writer_operation::CHANNEL | crate::protocol::writer_operation::METADATA | crate::protocol::writer_operation::ATTACHMENT | crate::protocol::writer_operation::START_ATTACHMENT) {
            return Err("Unsupported prepared operation".into());
        }
        *handle = Box::into_raw(Box::new(PreparedOperation {
            op,
            args: request(p, n)?,
            data: bytes(data, len)?.to_vec(),
        }));
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_operation_release(
    p: *mut PreparedOperation,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_prepared(
    p: *mut Writer,
    operation: *const PreparedOperation,
    data: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = writer_guard(out, |out| {
        let op = operation.as_ref().ok_or("Null operation")?;
        writer_control(
            p,
            op.op,
            &op.args,
            if op.op == crate::protocol::writer_operation::SCHEMA {
                &op.data
            } else {
                bytes(data, n)?
            },
            out,
        )
    });
    writer_result(p, status)
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_prepare(
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    handle: *mut *mut PreparedChannel,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(p, n)?;
        let schema = if v["schema"].is_null() {
            None
        } else {
            let s = &v["schema"];
            Some(Arc::new(mcap::Schema {
                id: number(s, "id")?.try_into()?,
                name: string(s, "name")?.into(),
                encoding: string(s, "encoding")?.into(),
                data: Cow::Owned(bytes(data, len)?.to_vec()),
            }))
        };
        *handle = Box::into_raw(Box::new(PreparedChannel(Arc::new(mcap::Channel {
            id: number(&v, "id")?.try_into()?,
            topic: string(&v, "topic")?.into(),
            message_encoding: string(&v, "encoding")?.into(),
            schema,
            metadata: map(&v["metadata"])?,
        }))));
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_release(p: *mut PreparedChannel, out: *mut Response) -> i32 {
    guard(out, |_| {
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
        Ok(crate::protocol::status::SUCCESS)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_full_message(
    handle: *mut Writer,
    channel: *const PreparedChannel,
    h: *const MessageHeader,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let c = channel.as_ref().ok_or("Null channel")?;
        let h = h.as_ref().ok_or("Null header")?;
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write(&mcap::Message {
                channel: c.0.clone(),
                sequence: h.sequence,
                log_time: h.log_time,
                publish_time: h.publish_time,
                data: Cow::Borrowed(bytes(p, n)?),
            })?;
        Ok(crate::protocol::status::SUCCESS)
    });
    writer_result(handle, status)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_private(
    handle: *mut Writer,
    opcode: u8,
    chunks: bool,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let opts = if chunks {
            enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
        } else {
            enumset::EnumSet::new()
        };
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write_private_record(opcode, bytes(p, n)?, opts)?;
        Ok(crate::protocol::status::SUCCESS)
    });
    writer_result(handle, status)
}
