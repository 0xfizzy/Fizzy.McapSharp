// Intentionally incompatible test-only library. Never stage this as a package native asset.
#[no_mangle]
pub extern "C" fn fm_abi_version() -> u32 { 0 }
