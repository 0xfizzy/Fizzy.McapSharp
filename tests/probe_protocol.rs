//! Test-only progress evidence shared by the FFI and registry probes.
use std::io::{self, Write};

pub const CAPACITY: usize = 8 * 1024 * 1024;
pub const INITIAL_HASH: u64 = 0xcbf29ce484222325;

pub fn emit(value: &str) {
    println!("MCAP_PROBE {value}");
    io::stdout().flush().unwrap();
}

pub fn start(mode: u32, length: usize) {
    emit(&format!("config v1 mcap=0.25.0 mode={mode} input={length} crc=true end=true capacity={CAPACITY}"));
}

pub fn next(mode: u32, ordinal: usize, hash: u64) {
    emit(&format!("next {mode} {ordinal} {hash:016x}"));
}

pub fn observe(hash: &mut u64, opcode: u8, data: &[u8]) {
    for byte in [opcode].into_iter().chain((data.len() as u64).to_le_bytes()).chain(data.iter().copied()) {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
}
