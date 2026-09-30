//! Budgeted codec adapters; format options match the upstream writer defaults.
use crate::{
    codec_memory::{self, CodecMemory},
    storage::{MemoryBudget, Reservation, ResourceCategory},
};
use std::{
    ffi::c_void,
    io::{self, Write},
    ptr,
    sync::Arc,
};
struct Output {
    data: Vec<u8>,
    _charge: Reservation,
}
impl Output {
    fn new(budget: &Arc<MemoryBudget>, size: usize) -> io::Result<Self> {
        let (mut data, charge) = crate::charged::bytes(budget, ResourceCategory::Writer, size)?;
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
    pub(crate) struct Encoder<W: Write> {
        writer: W,
        context: Context,
        output: Output,
    }
    impl<W: Write> Encoder<W> {
        pub fn new(
            writer: W,
            level: i32,
            threads: u32,
            budget: Arc<MemoryBudget>,
        ) -> io::Result<Self> {
            let memory = CodecMemory::new(budget.clone(), ResourceCategory::CodecEncoder)?;
            let raw = unsafe {
                ZSTD_createCCtx_advanced(CustomMem {
                    alloc: Some(codec_memory::allocate),
                    free: Some(codec_memory::free),
                    opaque: CodecMemory::opaque(&memory),
                })
            };
            if raw.is_null() {
                return Err(memory.error());
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
                    threads.try_into().map_err(io::Error::other)?,
                )
            })?;
            Ok(encoder)
        }
        fn check(&self, code: usize) -> io::Result<usize> {
            if let Some(e) = self.context.memory.take_error() {
                return Err(e);
            }
            if unsafe { sys::ZSTD_isError(code) } != 0 {
                return Err(io::Error::other(zstd::zstd_safe::get_error_name(code)));
            }
            Ok(code)
        }
        fn run(&mut self, data: &[u8], directive: sys::ZSTD_EndDirective) -> io::Result<()> {
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
                self.writer.write_all(&self.output.data[..output.pos])?;
                if input.pos == input.size
                    && (directive == sys::ZSTD_EndDirective::ZSTD_e_continue || left == 0)
                {
                    break;
                }
            }
            Ok(())
        }
        pub fn finish(mut self) -> (W, io::Result<()>) {
            let result = self.run(&[], sys::ZSTD_EndDirective::ZSTD_e_end);
            (self.writer, result)
        }
        pub fn into_inner(self) -> W {
            self.writer
        }
    }
    impl<W: Write> Write for Encoder<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.run(data, sys::ZSTD_EndDirective::ZSTD_e_continue)?;
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.run(&[], sys::ZSTD_EndDirective::ZSTD_e_flush)?;
            self.writer.flush()
        }
    }
}
#[cfg(feature = "lz4")]
pub(crate) mod lz4_encoder {
    use super::*;
    use lz4::liblz4::*;
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
    pub(crate) struct Encoder<W: Write> {
        writer: W,
        context: Context,
        output: Output,
        limit: usize,
    }
    impl<W: Write> Encoder<W> {
        pub fn new(writer: W, level: u32, budget: Arc<MemoryBudget>) -> io::Result<Self> {
            let memory = CodecMemory::new(budget.clone(), ResourceCategory::CodecEncoder)?;
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
                return Err(memory.error());
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
            let size = check_error(unsafe { LZ4F_compressBound(limit, &preferences) })?;
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
        fn emit(&mut self, code: usize) -> io::Result<usize> {
            if let Some(e) = self.context.memory.take_error() {
                return Err(e);
            }
            let len = check_error(code)?;
            self.context.memory.progress(0, len);
            self.writer.write_all(&self.output.data[..len])?;
            Ok(len)
        }
        pub fn finish(mut self) -> (W, io::Result<()>) {
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
    impl<W: Write> Write for Encoder<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
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
                self.emit(code)?;
                self.context.memory.progress(part.len(), 0);
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
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
            self.writer.flush()
        }
    }
}
