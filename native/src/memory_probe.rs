//! Test-only allocator observations, isolated to the measuring thread.
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! { static COUNTS: Cell<Option<(u64,u64)>> = const { Cell::new(None) }; }
struct Counting;
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn count(n: usize) {
    let _ = COUNTS.try_with(|c| {
        if let Some((calls, bytes)) = c.get() {
            c.set(Some((calls + 1, bytes + n as u64)));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count(n);
        System.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}
#[test]
fn memory_read_baseline() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let mut w = mcap::WriteOptions::new()
            .compression(compression)
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let c = w.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
        for sequence in 0..4096 {
            w.write_to_known_channel(
                &records::MessageHeader {
                    channel_id: c,
                    sequence,
                    log_time: sequence as u64,
                    publish_time: 0,
                },
                &[42; 1024],
            )
            .unwrap();
        }
        w.finish().unwrap();
        let data = w.into_inner().into_inner();
        unsafe {
            let mut h = ptr::null_mut();
            let mut r = Response::default();
            assert_eq!(
                buffer_reader::fm_buffer_reader_open(
                    5,
                    false,
                    data.as_ptr(),
                    data.len(),
                    &mut h,
                    &mut r
                ),
                0
            );
            let mut output = [0; 1024];
            let mut header = MessageHeader::default();
            COUNTS.with(|c| c.set(Some((0, 0))));
            let start = std::time::Instant::now();
            for _ in 0..4096 {
                assert_eq!(
                    buffer_reader::fm_buffer_reader_message(
                        h,
                        output.as_mut_ptr(),
                        output.len(),
                        &mut header,
                        &mut r
                    ),
                    0
                );
            }
            let elapsed = start.elapsed();
            let counts = COUNTS.with(|c| c.replace(None).unwrap());
            eprintln!(
                "memory-read {compression:?}: calls={}, requested_bytes={}, elapsed_us={}",
                counts.0,
                counts.1,
                elapsed.as_micros()
            );
            assert!(
                counts.0 < 1000,
                "Per-message native allocation regression: {counts:?}"
            );
            assert_eq!(
                (*h).delivery.stats.allocations,
                0,
                "Adequate destination must not allocate wrapper payload buffers"
            );
            eprintln!(
                "controlled_peak={}, wrapper_copied_bytes={}",
                (*h).delivery.stats.peak,
                (*h).delivery.stats.copied
            );
            buffer_reader::fm_buffer_reader_free(h);
        }
    }
}

#[test]
fn memory_writer_baseline() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        for buffered in [false, true] {
            for indexes in [false, true] {
                for chunk_size in [Some(65536), None] {
                    let mut w = mcap::WriteOptions::new()
                        .compression(compression)
                        .disable_seeking(buffered)
                        .emit_message_indexes(indexes)
                        .emit_chunk_indexes(indexes)
                        .chunk_size(chunk_size)
                        .create(memory::Measure::default())
                        .unwrap();
                    let c = w.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
                    COUNTS.with(|c| c.set(Some((0, 0))));
                    for sequence in 0..8192 {
                        w.write_to_known_channel(
                            &records::MessageHeader {
                                channel_id: c,
                                sequence,
                                log_time: sequence as u64,
                                publish_time: 0,
                            },
                            &[42; 1024],
                        )
                        .unwrap();
                    }
                    w.finish().unwrap();
                    let counts = COUNTS.with(|c| c.replace(None).unwrap());
                    eprintln!("memory-write {compression:?} buffered={buffered} indexes={indexes} chunk={chunk_size:?}: calls={} requested_bytes={}", counts.0, counts.1);
                }
            }
        }
    }
}

#[test]
fn memory_query_baseline() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let mut w = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(Some(65536))
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let c = w.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
        for sequence in 0..4096 {
            w.write_to_known_channel(
                &records::MessageHeader {
                    channel_id: c,
                    sequence,
                    log_time: (sequence % 64) as u64,
                    publish_time: 0,
                },
                &[42; 1024],
            )
            .unwrap();
        }
        w.finish().unwrap();
        let data = w.into_inner().into_inner();
        let summary = mcap::Summary::read(&data).unwrap().unwrap();
        COUNTS.with(|c| c.set(Some((0, 0))));
        let mut reader =
            sans_io::IndexedReader::new_with_options(&summary, Default::default()).unwrap();
        let mut count = 0;
        while let Some(event) = reader.next_event() {
            match event.unwrap() {
                sans_io::IndexedReadEvent::ReadChunkRequest { offset, length } => {
                    reader
                        .insert_chunk_record_data(
                            offset,
                            &data[offset as usize..offset as usize + length],
                        )
                        .unwrap();
                }
                sans_io::IndexedReadEvent::Message { .. } => count += 1,
            }
        }
        assert_eq!(count, 4096);
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        eprintln!(
            "memory-overlap-indexed {compression:?}: calls={} requested_bytes={}",
            counts.0, counts.1
        );
        let index = &summary.chunk_indexes[0];
        let entries = summary.read_message_indexes(&data, index).unwrap();
        let entry = entries.values().next().unwrap().last().unwrap();
        let key = buffer_reader::encode(records::Record::ChunkIndex(index.clone()))
            .unwrap()
            .1;
        for cached in [false, true] {
            let mut cache = random_access::ChunkCache::default();
            let mut output = [0; 1024];
            let mut h = MessageHeader::default();
            let mut r = Response::default();
            COUNTS.with(|c| c.set(Some((0, 0))));
            for _ in 0..128 {
                if cached {
                    unsafe {
                        assert_eq!(
                            cache
                                .read(
                                    &data,
                                    &summary,
                                    index,
                                    &key,
                                    entry.offset,
                                    1024 * 1024,
                                    output.as_mut_ptr(),
                                    output.len(),
                                    &mut h,
                                    &mut r
                                )
                                .unwrap(),
                            Some(0)
                        );
                    }
                } else {
                    summary.seek_message(&data, index, entry).unwrap();
                }
            }
            let counts = COUNTS.with(|c| c.replace(None).unwrap());
            eprintln!(
                "memory-random {compression:?} cached={cached}: calls={} requested_bytes={}",
                counts.0, counts.1
            );
        }
        COUNTS.with(|c| c.set(Some((0, 0))));
        let mut arena = sort_arena::Arena::default();
        for sequence in 0..4096 {
            arena
                .push(
                    MessageHeader {
                        sequence,
                        log_time: (sequence % 64) as u64,
                        ..Default::default()
                    },
                    &[42; 1024],
                    None,
                )
                .unwrap();
        }
        arena.sort(false);
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        eprintln!(
            "memory-sort {compression:?}: calls={} requested_bytes={} controlled_peak={}",
            counts.0, counts.1, arena.stats.peak
        );
    }
}
