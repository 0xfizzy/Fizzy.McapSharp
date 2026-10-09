//! Private C ABI. Borrowed memory crosses only explicit callback and lease boundaries.
mod protocol;
mod reader;
mod writer;
mod summary;
mod batch;
mod buffer_reader;
mod chunk_cache;
#[cfg(test)]
mod coverage_tests;
mod errors;
mod engine;
mod snapshot;
mod prepared_write;
mod record_access;
mod io;
mod lease;
mod memory;
mod sort_arena;
use io::Callbacks;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};
type Error = Box<dyn std::error::Error>;
type Outcome<T> = Result<T, Error>;
#[derive(Debug)]
pub(crate) struct OperationRestoreError {
    pub(crate) operation: Error,
    pub(crate) cleanup: std::io::Error,
}
impl std::fmt::Display for OperationRestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Operation and source-position restoration both failed")
    }
}
impl std::error::Error for OperationRestoreError {}
fn restored<T>(result: Outcome<T>, restore: std::io::Result<u64>) -> Outcome<T> {
    match (result, restore) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(operation), Err(cleanup)) => Err(Box::new(OperationRestoreError { operation, cleanup })),
    }
}
#[repr(C)]
#[derive(Default)]
pub struct Response {
    json: *mut u8,
    json_len: usize,
    data: *mut u8,
    data_len: usize,
    value: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct MessageHeader {
    channel_id: u16,
    reserved: u16,
    sequence: u32,
    log_time: u64,
    publish_time: u64,
}
fn buffer(v: Vec<u8>) -> (*mut u8, usize) {
    if v.is_empty() {
        return (ptr::null_mut(), 0);
    }
    let b = v.into_boxed_slice();
    let n = b.len();
    (Box::into_raw(b) as *mut u8, n)
}
fn respond(out: &mut Response, h: Vec<u8>, d: Vec<u8>, v: u64) {
    // Also release a partially published response when guard replaces it with an error.
    unsafe {
        fm_buffer_free(out.json, out.json_len);
        fm_buffer_free(out.data, out.data_len);
    }
    *out = Response::default();
    (out.json, out.json_len) = buffer(h);
    (out.data, out.data_len) = buffer(d);
    out.value = v;
}
fn guard(out: *mut Response, f: impl FnOnce(&mut Response) -> Outcome<i32>) -> i32 {
    if out.is_null() {
        return crate::protocol::status::ERROR;
    }
    let out = unsafe { &mut *out };
    *out = Response::default();
    match catch_unwind(AssertUnwindSafe(|| f(out))) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let encoded = catch_unwind(AssertUnwindSafe(|| errors::encode(e.as_ref())))
                .unwrap_or_else(|_| b"Native MCAP panic; operation failed".to_vec());
            respond(out, encoded, vec![], 0);
            crate::protocol::status::ERROR
        }
        Err(_) => {
            respond(
                out,
                b"Native MCAP panic; operation failed".to_vec(),
                vec![],
                0,
            );
            crate::protocol::status::ERROR
        }
    }
}
unsafe fn bytes<'a>(p: *const u8, n: usize) -> Outcome<&'a [u8]> {
    if n == 0 {
        return Ok(&[]);
    }
    if p.is_null() || n > isize::MAX as usize {
        return Err("Invalid input buffer".into());
    }
    Ok(slice::from_raw_parts(p, n))
}
unsafe fn request(p: *const u8, n: usize) -> Outcome<Value> {
    Ok(serde_json::from_slice(bytes(p, n)?)?)
}
fn string<'a>(v: &'a Value, k: &str) -> Outcome<&'a str> {
    v[k].as_str()
        .ok_or_else(|| format!("Missing string: {k}").into())
}
fn number(v: &Value, k: &str) -> Outcome<u64> {
    v[k].as_u64()
        .ok_or_else(|| format!("Missing integer: {k}").into())
}
fn map(v: &Value) -> Outcome<BTreeMap<String, String>> {
    Ok(serde_json::from_value(v.clone())?)
}
#[no_mangle]
pub extern "C" fn fm_abi_version() -> u32 {
    15
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(p, n)))
    }
}

// These are private C ABI layouts on all supported 64-bit targets, independent of Rust record layouts.
const _: () = assert!(std::mem::size_of::<MessageHeader>() == 24);
const _: () = assert!(std::mem::offset_of!(MessageHeader, log_time) == 8);
const _: () = assert!(std::mem::size_of::<Response>() == 40);
const _: () = assert!(std::mem::size_of::<Callbacks>() == 48);

#[cfg(test)]
mod memory_probe;
