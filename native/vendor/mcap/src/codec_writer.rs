//! Budgeted codec adapters; format options match the upstream writer defaults.
use crate::{
    codec_memory::{self, CodecMemory},
    storage::{Reservation, ResourceCategory},
    McapError, McapResult,
};
use std::{
    ffi::c_void,

    ptr,
};
use crate::io_utils::McapWrite;
#[cfg(test)]
use std::io::Write;
struct Output {
    data: Vec<u8>,
    _charge: Reservation,
}
impl Output {
    fn new(budget: &crate::storage::BudgetRef, size: usize) -> McapResult<Self> {
        let (mut data, charge) = crate::charged::vector_fixed::<u8>(budget, ResourceCategory::Writer, size)
            .map_err(crate::storage::StorageFailure::terminal)?;
        data.resize(size, 0);
        Ok(Self {
            data,
            _charge: charge,
        })
    }
}
#[cfg(feature = "zstd")]
pub(crate) mod zstd_encoder {
    use super::*;
    use zstd::zstd_safe::zstd_sys as sys;
    #[repr(C)]
    struct CustomMem {
        alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
        free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        opaque: *mut c_void,
    }
    extern "C" {
        fn ZSTD_createCCtx_advanced(memory: CustomMem) -> *mut sys::ZSTD_CCtx;
    }
    struct Context {
        raw: *mut sys::ZSTD_CCtx,
        memory: crate::charged::ChargedBox<CodecMemory>,
    }
    unsafe impl Send for Context {}
    impl Drop for Context {
        fn drop(&mut self) {
            unsafe {
                sys::ZSTD_freeCCtx(self.raw);
            }
        }
    }
    pub(crate) struct Encoder<W: McapWrite> {
        writer: W,
        context: Context,
        output: Output,
    }
    impl<W: McapWrite> Encoder<W> {
        pub fn new(
            writer: W,
            level: i32,
            threads: u32,
            budget: crate::storage::BudgetRef,
        ) -> McapResult<Self> {
            let memory = CodecMemory::new_fixed(budget.clone(), ResourceCategory::CodecEncoder)?;
            let raw = unsafe {
                ZSTD_createCCtx_advanced(CustomMem {
                    alloc: Some(codec_memory::allocate),
                    free: Some(codec_memory::free),
                    opaque: CodecMemory::opaque(&memory),
                })
            };
            if raw.is_null() {
                return Err(memory.fixed_error().into());
            }
            let context = Context { raw, memory };
            let encoder = Self {
                writer,
                context,
                output: Output::new(&budget, unsafe { sys::ZSTD_CStreamOutSize() })?,
            };
            encoder.check(unsafe {
                sys::ZSTD_CCtx_setParameter(
                    raw,
                    sys::ZSTD_cParameter::ZSTD_c_compressionLevel,
                    level,
                )
            })?;
            encoder.check(unsafe {
                sys::ZSTD_CCtx_setParameter(
                    raw,
                    sys::ZSTD_cParameter::ZSTD_c_nbWorkers,
                    threads.try_into().map_err(|_| McapError::StaticIoError("out of range integral type conversion attempted"))?,
                )
            })?;
            Ok(encoder)
        }
        fn check(&self, code: usize) -> McapResult<usize> {
            if let Some(e) = self.context.memory.take_failure() {
                return Err(e.into());
            }
            if unsafe { sys::ZSTD_isError(code) } != 0 {
                return Err(McapError::StaticIoError(zstd::zstd_safe::get_error_name(code)));
            }
            Ok(code)
        }
        fn run(&mut self, data: &[u8], directive: sys::ZSTD_EndDirective) -> McapResult<()> {
            let mut input = sys::ZSTD_inBuffer {
                src: data.as_ptr().cast(),
                size: data.len(),
                pos: 0,
            };
            loop {
                let mut output = sys::ZSTD_outBuffer {
                    dst: self.output.data.as_mut_ptr().cast(),
                    size: self.output.data.len(),
                    pos: 0,
                };
                let before = input.pos;
                let code = unsafe {
                    sys::ZSTD_compressStream2(self.context.raw, &mut output, &mut input, directive)
                };
                self.context.memory.progress(input.pos - before, output.pos);
                let left = self.check(code)?;
                self.writer.write_all_mcap(&self.output.data[..output.pos])?;
                if input.pos == input.size
                    && (directive == sys::ZSTD_EndDirective::ZSTD_e_continue || left == 0)
                {
                    break;
                }
            }
            Ok(())
        }
        pub fn finish(mut self) -> (W, McapResult<()>) {
            let result = self.run(&[], sys::ZSTD_EndDirective::ZSTD_e_end);
            (self.writer, result)
        }
        pub fn into_inner(self) -> W {
            self.writer
        }
    }
    impl<W: McapWrite> Encoder<W> {
        pub fn write_all(&mut self, data: &[u8]) -> McapResult<()> { self.write(data).map(|_| ()) }
        pub fn write(&mut self, data: &[u8]) -> McapResult<usize> {
            self.run(data, sys::ZSTD_EndDirective::ZSTD_e_continue)?;
            Ok(data.len())
        }
        pub fn flush(&mut self) -> McapResult<()> {
            self.run(&[], sys::ZSTD_EndDirective::ZSTD_e_flush)?;
            self.writer.flush_mcap()
        }
    }
}
#[cfg(feature = "lz4")]
pub(crate) mod lz4_encoder {
    use super::*;
    use lz4::liblz4::*;
    fn checked(code: usize) -> McapResult<usize> {
        if unsafe { LZ4F_isError(code) } != 0 {
            // Public API returns a name from the pinned codec's static table.
            let name: &'static str = unsafe { std::ffi::CStr::from_ptr(LZ4F_getErrorName(code)) }
                .to_str().unwrap_or("invalid LZ4 error name");
            return Err(McapError::Lz4Error(name));
        }
        Ok(code)
    }

    #[repr(C)]
    struct CustomMem {
        alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
        calloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
        free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        opaque: *mut c_void,
    }
    extern "C" {
        fn LZ4F_createCompressionContext_advanced(
            memory: CustomMem,
            version: u32,
        ) -> LZ4FCompressionContext;
    }
    struct Context {
        raw: LZ4FCompressionContext,
        memory: crate::charged::ChargedBox<CodecMemory>,
    }
    impl Drop for Context {
        fn drop(&mut self) {
            unsafe {
                LZ4F_freeCompressionContext(self.raw);
            }
        }
    }
    pub(crate) struct Encoder<W: McapWrite> {
        writer: W,
        context: Context,
        output: Output,
        limit: usize,
    }
    impl<W: McapWrite> Encoder<W> {
        pub fn new(writer: W, level: u32, budget: crate::storage::BudgetRef) -> McapResult<Self> {
            let memory = CodecMemory::new_fixed(budget.clone(), ResourceCategory::CodecEncoder)?;
            let raw = unsafe {
                LZ4F_createCompressionContext_advanced(
                    CustomMem {
                        alloc: Some(codec_memory::allocate),
                        calloc: Some(codec_memory::calloc),
                        free: Some(codec_memory::free),
                        opaque: CodecMemory::opaque(&memory),
                    },
                    LZ4F_VERSION,
                )
            };
            if raw.0.is_null() {
                return Err(memory.fixed_error().into());
            }
            let context = Context { raw, memory };
            let preferences = LZ4FPreferences {
                frame_info: LZ4FFrameInfo {
                    block_size_id: BlockSize::Default,
                    block_mode: BlockMode::Linked,
                    content_checksum_flag: ContentChecksum::ChecksumEnabled,
                    content_size: 0,
                    frame_type: FrameType::Frame,
                    dict_id: 0,
                    block_checksum_flag: BlockChecksum::NoBlockChecksum,
                },
                compression_level: level,
                auto_flush: 0,
                favor_dec_speed: 0,
                reserved: [0; 3],
            };
            let limit = BlockSize::Default.get_size();
            let size = checked(unsafe { LZ4F_compressBound(limit, &preferences) })?;
            let mut encoder = Self {
                writer,
                context,
                output: Output::new(&budget, size)?,
                limit,
            };
            let code = unsafe {
                LZ4F_compressBegin(
                    encoder.context.raw,
                    encoder.output.data.as_mut_ptr(),
                    size,
                    &preferences,
                )
            };
            encoder.emit(code)?;
            Ok(encoder)
        }
        fn emit(&mut self, code: usize) -> McapResult<usize> {
            self.emit_consumed(code, 0)
        }
        fn emit_consumed(&mut self, code: usize, consumed: usize) -> McapResult<usize> {
            if let Some(e) = self.context.memory.take_failure() {
                return Err(e.into());
            }
            let len = checked(code)?;
            self.context.memory.progress(consumed, len);
            self.writer.write_all_mcap(&self.output.data[..len])?;
            Ok(len)
        }
        pub fn finish(mut self) -> (W, McapResult<()>) {
            let code = unsafe {
                LZ4F_compressEnd(
                    self.context.raw,
                    self.output.data.as_mut_ptr(),
                    self.output.data.len(),
                    ptr::null(),
                )
            };
            let result = self.emit(code).map(|_| ());
            (self.writer, result)
        }
        pub fn into_inner(self) -> W {
            self.writer
        }
    }
    impl<W: McapWrite> Encoder<W> {
        pub fn write_all(&mut self, data: &[u8]) -> McapResult<()> { self.write(data).map(|_| ()) }
        pub fn write(&mut self, data: &[u8]) -> McapResult<usize> {
            for part in data.chunks(self.limit) {
                let code = unsafe {
                    LZ4F_compressUpdate(
                        self.context.raw,
                        self.output.data.as_mut_ptr(),
                        self.output.data.len(),
                        part.as_ptr(),
                        part.len(),
                        ptr::null(),
                    )
                };
                self.emit_consumed(code, part.len())?;
            }
            Ok(data.len())
        }
        pub fn flush(&mut self) -> McapResult<()> {
            loop {
                let code = unsafe {
                    LZ4F_flush(
                        self.context.raw,
                        self.output.data.as_mut_ptr(),
                        self.output.data.len(),
                        ptr::null(),
                    )
                };
                if self.emit(code)? == 0 {
                    break;
                }
            }
            self.writer.flush_mcap()
        }
    }
}

#[cfg(all(test, feature = "lz4"))]
mod tests {
    use super::*;
    #[test]
    fn lz4_counts_consumed_input_before_output_failure() {
        struct RejectOutput(bool);
        impl Write for RejectOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0 { return Err(std::io::ErrorKind::BrokenPipe.into()); }
                self.0 = true; // Accept the frame header, reject the first data block.
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        impl McapWrite for RejectOutput {
            fn write_mcap(&mut self, bytes: &[u8]) -> McapResult<usize> { Ok(self.write(bytes)?) }
            fn flush_mcap(&mut self) -> McapResult<()> { Ok(self.flush()?) }
        }
        let domain = crate::storage::BudgetRef::new(Default::default()).unwrap();
        let mut encoder = lz4_encoder::Encoder::new(RejectOutput(false), 0, domain.clone()).unwrap();
        let before = domain.detailed_statistics().flow;
        assert!(matches!(encoder.write_all(&[37;65536]), Err(McapError::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe));
        let after = domain.detailed_statistics().flow;
        assert_eq!(after.encoded_input - before.encoded_input, 65536);
        assert!(after.encoded_output > before.encoded_output);
        drop(encoder);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
