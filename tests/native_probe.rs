//! Standalone FFI driver: rustc uses the repository's pinned toolchain, no extra crates.
use std::{env, ffi::c_void, fs, mem, ptr, slice};
#[repr(C)]
#[derive(Default)]
struct Response { json: *mut u8, json_len: usize, data: *mut u8, data_len: usize, value: u64 }
#[repr(C)]
struct Header { channel: u16, reserved: u16, sequence: u32, log_time: u64, publish_time: u64 }
extern "C" {
    fn fm_abi_version() -> u32;
    fn fm_buffer_free(p: *mut u8, n: usize);
    fn fm_buffer_reader_open(mode: u32, ignore: bool, p: *const u8, n: usize, h: *mut *mut c_void, r: *mut Response) -> i32;
    fn fm_buffer_reader_next(h: *mut c_void, p: *mut u8, n: usize, op: *mut u8, r: *mut Response) -> i32;
    fn fm_buffer_reader_free(h: *mut c_void);
    fn fm_parse_record(op: u8, p: *const u8, n: usize, r: *mut Response) -> i32;
    fn fm_snapshot_bytes(p: *const u8, n: usize, h: *mut *mut c_void, r: *mut Response) -> i32;
    fn fm_snapshot_summary(h: *mut c_void, r: *mut Response) -> i32;
    fn fm_snapshot_call(h: *mut c_void, op: u32, index: *const u8, length: usize, time: u64, offset: u64,
        dest: *mut u8, capacity: usize, header: *mut Header, r: *mut Response) -> i32;
    fn fm_snapshot_free(h: *mut c_void);
}
unsafe fn release(r: &mut Response) {
    let panic = r.json_len > 0 && String::from_utf8_lossy(slice::from_raw_parts(r.json, r.json_len)).contains("Native MCAP panic");
    fm_buffer_free(r.json, r.json_len); fm_buffer_free(r.data, r.data_len);
    *r = Response::default();
    assert!(!panic, "FFI caught a panic");
}
fn main() {
    assert_eq!(mem::size_of::<Response>(), 40);
    assert_eq!(mem::size_of::<Header>(), 24);
    assert_eq!(mem::offset_of!(Header, log_time), 8);
    let args: Vec<_> = env::args().collect();
    let data = fs::read(&args[1]).unwrap();
    let repeats = args.get(2).map(|s| s.parse::<usize>().unwrap()).unwrap_or(1);
    unsafe {
        assert_eq!(fm_abi_version(), 10);
        for _ in 0..repeats {
            let mut indexes = Vec::new();
            for mode in [0, 2, 4, 5] {
                let mut h = ptr::null_mut(); let mut r = Response::default();
                let status = fm_buffer_reader_open(mode, false, data.as_ptr(), data.len(), &mut h, &mut r);
                release(&mut r);
                if status < 0 { assert!(h.is_null()); continue; }
                let mut b = vec![0; 8 * 1024 * 1024]; let mut op = 0;
                loop {
                    let status = fm_buffer_reader_next(h, b.as_mut_ptr(), b.len(), &mut op, &mut r);
                    let length = r.value as usize; release(&mut r);
                    if status != 0 { break; }
                    assert!(length <= b.len());
                    if mode == 0 && [8, 10, 13].contains(&op) { indexes.push((op, b[..length].to_vec())); }
                    let parsed = fm_parse_record(op, b.as_ptr(), length, &mut r); release(&mut r);
                    assert_eq!(parsed, 0, "Reader emitted an invalid record");
                }
                fm_buffer_reader_free(h);
            }
            let mut h = ptr::null_mut(); let mut r = Response::default();
            let status = fm_snapshot_bytes(data.as_ptr(), data.len(), &mut h, &mut r); release(&mut r);
            if status == 0 {
                for op in [1, 7] {
                    assert!(fm_snapshot_call(h, op, ptr::null(), 0, 0, 0, ptr::null_mut(), 0, ptr::null_mut(), &mut r) < 0);
                    release(&mut r);
                }
                fm_snapshot_summary(h, &mut r); release(&mut r);
                let mut b = vec![0; 8 * 1024 * 1024];
                for (opcode, index) in &indexes {
                    let operations: &[u32] = match opcode { 8 => &[5, 8], 10 => &[4], _ => &[3] };
                    for op in operations {
                        let mut header = Header { channel: 0, reserved: 0, sequence: 0, log_time: 0, publish_time: 0 };
                        let status = fm_snapshot_call(h, *op, index.as_ptr(), index.len(), 0, 0, b.as_mut_ptr(), b.len(), &mut header, &mut r);
                        release(&mut r);
                        if status < 0 { break; }
                    }
                }
                fm_snapshot_free(h);
            }
            else { assert!(h.is_null()); }
        }
    }
}
