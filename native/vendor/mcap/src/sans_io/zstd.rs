use crate::{
    codec_memory::{self, CodecMemory},
    storage::{MemoryBudget, ResourceCategory},
};
use crate::{
    sans_io::decompressor::{DecompressResult, Decompressor},
    McapResult,
};
use std::{ffi::c_void, sync::Arc};
use zstd::zstd_safe::zstd_sys as sys;
#[repr(C)]
struct CustomMem {
    alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    opaque: *mut c_void,
}
extern "C" {
    fn ZSTD_createDCtx_advanced(memory: CustomMem) -> *mut sys::ZSTD_DCtx;
}
pub struct ZstdDecoder {
    s: *mut sys::ZSTD_DCtx,
    memory: Arc<CodecMemory>,
    need: usize,
    started: bool,
}
unsafe impl Send for ZstdDecoder {}
impl ZstdDecoder {
    pub(crate) fn with_budget(budget: Arc<MemoryBudget>) -> McapResult<Self> {
        let memory = CodecMemory::new(budget, ResourceCategory::CodecDecoder);
        let s = unsafe {
            ZSTD_createDCtx_advanced(CustomMem {
                alloc: Some(codec_memory::allocate),
                free: Some(codec_memory::free),
                opaque: CodecMemory::opaque(&memory),
            })
        };
        if s.is_null() {
            return Err(memory.error().into());
        }
        let mut decoder = Self {
            s,
            memory,
            need: 0,
            started: false,
        };
        decoder.need = decoder.check(unsafe { sys::ZSTD_initDStream(s) })?;
        Ok(decoder)
    }
    fn check(&self, code: usize) -> McapResult<usize> {
        if let Some(error) = self.memory.take_error() {
            return Err(error.into());
        }
        if unsafe { sys::ZSTD_isError(code) } != 0 {
            return Err(crate::McapError::DecompressionError(
                zstd::zstd_safe::get_error_name(code).into(),
            ));
        }
        Ok(code)
    }
}
impl Drop for ZstdDecoder {
    fn drop(&mut self) {
        unsafe {
            sys::ZSTD_freeDCtx(self.s);
        }
    }
}
impl Decompressor for ZstdDecoder {
    fn next_read_size(&self) -> usize {
        self.need
    }
    fn decompress(&mut self, src: &[u8], dst: &mut [u8]) -> McapResult<DecompressResult> {
        if !self.started {
            self.memory.decode_event(false);
            self.started = true;
        }
        let mut input = sys::ZSTD_inBuffer {
            src: src.as_ptr().cast(),
            size: src.len(),
            pos: 0,
        };
        let mut output = sys::ZSTD_outBuffer {
            dst: dst.as_mut_ptr().cast(),
            size: dst.len(),
            pos: 0,
        };
        let code = unsafe { sys::ZSTD_decompressStream(self.s, &mut output, &mut input) };
        self.memory.progress(input.pos, output.pos);
        self.need = self.check(code)?;
        if self.need == 0 {
            self.memory.decode_event(true);
            self.started = false;
        }
        Ok(DecompressResult {
            consumed: input.pos,
            wrote: output.pos,
        })
    }
    fn reset(&mut self) -> McapResult<()> {
        self.check(unsafe {
            sys::ZSTD_DCtx_reset(self.s, sys::ZSTD_ResetDirective::ZSTD_reset_session_only)
        })?;
        self.started = false;
        Ok(())
    }
    fn name(&self) -> &'static str {
        "zstd"
    }
}
