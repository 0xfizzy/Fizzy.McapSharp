use crate::sans_io::decompressor::{DecompressResult, Decompressor};
use crate::McapResult;
use crate::{
    codec_memory::{self, CodecMemory},
    storage::ResourceCategory,
};
use std::{ffi::c_void, ptr};
#[repr(C)]
pub(crate) struct CustomMem {
    pub alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub calloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub opaque: *mut c_void,
}
extern "C" {
    fn LZ4F_createDecompressionContext_advanced(
        memory: CustomMem,
        version: u32,
    ) -> LZ4FDecompressionContext;
}

use lz4::liblz4::{
    LZ4F_isError, LZ4F_getErrorName, LZ4FDecompressionContext, LZ4F_decompress, LZ4F_freeDecompressionContext,
    LZ4F_resetDecompressionContext, LZ4F_VERSION,
};

/// A Decompressor wrapper for LZ4 streaming decompression.
pub struct Lz4Decoder {
    c: LZ4FDecompressionContext,
    memory: crate::charged::ChargedBox<CodecMemory>,
    next_read_size: usize,
    started: bool,
}

impl Lz4Decoder {
    pub(crate) fn with_budget(budget: crate::storage::BudgetRef) -> McapResult<Self> {
        let memory = CodecMemory::new_fixed(budget, ResourceCategory::CodecDecoder)?;
        let context = unsafe {
            LZ4F_createDecompressionContext_advanced(
                CustomMem {
                    alloc: Some(codec_memory::allocate),
                    calloc: Some(codec_memory::calloc),
                    free: Some(codec_memory::free),
                    opaque: CodecMemory::opaque(&memory),
                },
                LZ4F_VERSION,
            )
        };
        if context.0.is_null() {
            return Err(memory.fixed_error().into());
        }
        Ok(Lz4Decoder {
            c: context,
            memory,
            started: false,
            next_read_size: 13, // min frame size
        })
    }
}

impl Drop for Lz4Decoder {
    fn drop(&mut self) {
        unsafe { LZ4F_freeDecompressionContext(self.c) };
    }
}

impl Decompressor for Lz4Decoder {
    fn next_read_size(&self) -> usize {
        self.next_read_size
    }

    fn decompress(&mut self, src: &[u8], dst: &mut [u8]) -> McapResult<DecompressResult> {
        if !self.started {
            self.memory.decode_event(false);
            self.started = true;
        }
        let mut dst_size = dst.len();
        let mut src_size = src.len();
        let code = unsafe {
            LZ4F_decompress(
                self.c,
                dst.as_mut_ptr(),
                &mut dst_size,
                src.as_ptr(),
                &mut src_size,
                ptr::null(),
            )
        };
        self.memory.progress(src_size, dst_size);
        if let Some(error) = self.memory.take_failure() {
            return Err(error.into());
        }
        if unsafe { LZ4F_isError(code) } != 0 {
            // The pinned public LZ4F API returns a string from its static error
            // table. No context memory or private codec layout is borrowed.
            let name: &'static str = unsafe { std::ffi::CStr::from_ptr(LZ4F_getErrorName(code)) }
                .to_str().unwrap_or("invalid LZ4 error name");
            return Err(crate::McapError::Lz4Error(name));
        }
        let need = code;
        self.next_read_size = need;
        if need == 0 {
            self.memory.decode_event(true);
            self.started = false;
        }
        Ok(DecompressResult {
            consumed: src_size,
            wrote: dst_size,
        })
    }

    fn reset(&mut self) -> McapResult<()> {
        unsafe { LZ4F_resetDecompressionContext(self.c) };
        self.started = false;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "lz4"
    }
}
