//! Test-only evidence for binding delivery, not a production memory ledger.
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::sync::Arc;
thread_local! { static CALLS: Cell<Option<usize>> = const { Cell::new(None) }; }
thread_local! {
    static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
    // Preallocated only by lifetime diagnostics; allocator callbacks never grow this Vec.
    static LIVE: RefCell<Option<Vec<(usize, usize)>>> = const { RefCell::new(None) };
    static OVERFLOW: Cell<bool> = const { Cell::new(false) };
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static PEAK_BYTES: Cell<usize> = const { Cell::new(0) };
}
struct Probe;
#[global_allocator]
static ALLOCATOR: Probe = Probe;
fn count(size: usize) {
    let _ = CALLS.try_with(|c| {
        if let Some(n) = c.get() {
            c.set(Some(n + 1));
            let _ = ALLOCATED_BYTES.try_with(|b| b.set(b.get() + size));
        }
    });
}
fn track(pointer: *mut u8, size: usize) {
    if pointer.is_null() {
        return;
    }
    let _ = LIVE.try_with(|v| {
        if let Ok(mut v) = v.try_borrow_mut() {
            if let Some(v) = v.as_mut() {
                if v.len() < v.capacity() {
                    v.push((pointer as usize, size));
                    LIVE_BYTES.with(|b| {
                        b.set(b.get() + size);
                        PEAK_BYTES.with(|peak| peak.set(peak.get().max(b.get())));
                    });
                } else {
                    let _ = OVERFLOW.try_with(|o| o.set(true));
                }
            }
        }
    });
}
fn untrack(pointer: *mut u8) {
    let _ = LIVE.try_with(|v| {
        if let Ok(mut v) = v.try_borrow_mut() {
            if let Some(v) = v.as_mut() {
                if let Some(i) = v.iter().position(|(p, _)| *p == pointer as usize) {
                    let (_, size) = v.swap_remove(i);
                    LIVE_BYTES.with(|b| b.set(b.get() - size));
                }
            }
        }
    });
}
struct Measurement;
impl Measurement {
    fn start(track_live: bool) -> Self {
        if track_live {
            LIVE.with(|v| *v.borrow_mut() = Some(Vec::with_capacity(16384)));
        }
        OVERFLOW.with(|v| v.set(false));
        LIVE_BYTES.with(|v| v.set(0));
        PEAK_BYTES.with(|v| v.set(0));
        ALLOCATED_BYTES.with(|v| v.set(0));
        CALLS.with(|v| v.set(Some(0)));
        Self
    }
    fn totals(&self) -> (usize, usize) {
        (
            CALLS.with(|v| v.get().unwrap()),
            ALLOCATED_BYTES.with(Cell::get),
        )
    }
    fn owner(&self, data: &[u8]) -> (usize, usize) {
        let pointer = data.as_ptr() as usize;
        LIVE.with(|v| {
            v.borrow()
                .as_ref()
                .unwrap()
                .iter()
                .copied()
                .find(|(p, n)| pointer >= *p && pointer + data.len() <= p + n)
                .expect("storage allocation must be tracked")
        })
    }
}

// Compare the former eager tree with the real completion/response path. The
// counting sink excludes file contents; all owners are created under the probe.
#[test]
fn writer_summary_profile() {
    unsafe extern "C" fn read(
        _: *mut std::ffi::c_void,
        _: *mut u8,
        _: usize,
        _: *mut usize,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn write(p: *mut std::ffi::c_void, _: *const u8, n: usize) -> i32 {
        let sink = &mut *p.cast::<std::io::Cursor<u64>>();
        sink.set_position(sink.position() + n as u64);
        *sink.get_mut() = (*sink.get_ref()).max(sink.position());
        0
    }
    unsafe extern "C" fn seek(p: *mut std::ffi::c_void, n: i64, origin: i32, out: *mut u64) -> i32 {
        let sink = &mut *p.cast::<std::io::Cursor<u64>>();
        let base = match origin {
            0 => 0,
            1 => sink.position(),
            _ => *sink.get_ref(),
        };
        let position = base as i128 + n as i128;
        if position < 0 || position > u64::MAX as i128 {
            return -1;
        }
        sink.set_position(position as u64);
        *out = position as u64;
        0
    }
    unsafe extern "C" fn flush(_: *mut std::ffi::c_void) -> i32 {
        0
    }
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        for seekable in [true, false] {
            let mut old_retained = 0;
            let mut old_peak = 0;
            for eager in [true, false] {
                let mut sink = std::io::Cursor::new(0u64);
                let probe = Measurement::start(true);
                let output = Output::Stream(Callbacks {
                    context: (&mut sink as *mut std::io::Cursor<u64>).cast(),
                    read,
                    write,
                    seek,
                    flush,
                    seekable: seekable as u32,
                });
                let mut upstream = mcap::WriteOptions::new()
                    .compression(compression)
                    .disable_seeking(!seekable)
                    .chunk_size(None)
                    .create(output)
                    .unwrap();
                let schema = upstream
                    .add_schema("schema\"雪", "raw", &[1, 2, 3])
                    .unwrap();
                let a = upstream
                    .add_channel(schema, "a", "raw", &BTreeMap::new())
                    .unwrap();
                let b = upstream
                    .add_channel(0, "b", "raw", &BTreeMap::new())
                    .unwrap();
                let mut writing = [0usize; 4];
                for i in 0..64 {
                    upstream
                        .write_to_known_channel(
                            &records::MessageHeader {
                                channel_id: if i % 2 == 0 { a } else { b },
                                sequence: i,
                                log_time: i as u64,
                                publish_time: u64::MAX,
                            },
                            &[42; 1024],
                        )
                        .unwrap();
                    upstream.flush().unwrap();
                    if (i + 1) % 16 == 0 {
                        writing[(i as usize) / 16] = LIVE_BYTES.with(Cell::get);
                    }
                }
                let before = LIVE_BYTES.with(Cell::get);
                PEAK_BYTES.with(|v| v.set(before));
                let mut holder = Writer {
                    inner: Some(upstream),
                    completed_output: None,
                    failed: false,
                    recoverable_errors: 31,
                    attachment: None,
                    native_summary: None,
                };
                let tree = if eager {
                    let summary = holder.inner.as_mut().unwrap().finish().unwrap();
                    let tree = summary_json(&summary);
                    holder.native_summary = Some(Arc::new(summary));
                    holder.completed_output = Some(holder.inner.take().unwrap().into_inner());
                    holder.completed_output.as_mut().unwrap().flush().unwrap();
                    Some(tree)
                } else {
                    unsafe {
                        writer_control(&mut holder, 7, &Value::Null, &[], &mut Response::default())
                            .unwrap();
                    }
                    None
                };
                let retained = LIVE_BYTES.with(Cell::get);
                let peak = PEAK_BYTES.with(Cell::get);
                let (completion_allocations, completion_allocated_bytes) = probe.totals();
                assert!(holder.inner.is_none());
                assert_eq!(
                    Arc::strong_count(holder.native_summary.as_ref().unwrap()),
                    1
                );
                let summary = holder.native_summary.as_ref().unwrap();
                let bytes = writer_summary_bytes(summary).unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes).unwrap(),
                    summary_json(summary)
                );
                drop(bytes);
                let mut response = Response::default();
                PEAK_BYTES.with(|v| v.set(retained));
                let response_live;
                unsafe {
                    writer_control(&mut holder, 12, &Value::Null, &[], &mut response).unwrap();
                    response_live = LIVE_BYTES.with(Cell::get);
                    fm_buffer_free(response.json, response.json_len);
                }
                let response_peak = PEAK_BYTES.with(Cell::get);
                assert_eq!(LIVE_BYTES.with(Cell::get), retained);
                if eager {
                    old_retained = retained;
                    old_peak = peak;
                } else {
                    assert!(retained < old_retained);
                    assert!(peak < old_peak);
                }
                drop(tree);
                drop(holder);
                assert_eq!(LIVE_BYTES.with(Cell::get), 0);
                let (allocations, allocated_bytes) = probe.totals();
                drop(probe);
                let growth_per_chunk = (writing[3] - writing[0]) as f64 / 48.0;
                println!("writer-summary compression={compression:?} seekable={seekable} eager={eager} chunks=64 writing_samples={writing:?} growth_bytes_per_chunk={growth_per_chunk} writing_live={before} completed_live={retained} completion_peak={peak} completion_allocations={completion_allocations} completion_allocated_bytes={completion_allocated_bytes} response_live={response_live} response_peak={response_peak} allocations={allocations} allocated_bytes={allocated_bytes}");
            }
        }
    }
    let empty = mcap::Summary::default();
    assert_eq!(
        serde_json::from_slice::<Value>(&writer_summary_bytes(&empty).unwrap()).unwrap(),
        summary_json(&empty)
    );
}
impl Drop for Measurement {
    fn drop(&mut self) {
        CALLS.with(|v| v.set(None));
        LIVE.with(|v| *v.borrow_mut() = None);
        assert!(
            !OVERFLOW.with(Cell::get),
            "diagnostic allocation registry overflow"
        );
    }
}
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        let p = System.alloc(layout);
        track(p, layout.size());
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        let p = System.alloc_zeroed(layout);
        track(p, layout.size());
        p
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        count(n);
        let next = System.realloc(p, layout, n);
        if !next.is_null() {
            untrack(p);
            track(next, n);
        }
        next
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        untrack(p);
        System.dealloc(p, layout)
    }
}
#[test]
fn pending_delivery_preserves_pointer_and_allocates_nothing() {
    let source = Arc::new(vec![42; 70000]);
    let shared = mcap::storage::SharedBytes::external(source.clone(), 0..source.len());
    let mut delivery = memory::Delivery::default();
    delivery.shared = Some(shared.clone());
    let mut target = vec![0; source.len()];
    unsafe {
        CALLS.with(|c| c.set(Some(0)));
        assert_eq!(
            delivery
                .deliver(shared.as_ref(), ptr::null_mut(), 0)
                .unwrap(),
            2
        );
        for _ in 0..100 {
            assert_eq!(delivery.retry(ptr::null_mut(), 0).unwrap(), 2);
            assert_eq!(delivery.bytes().as_ptr(), source.as_ptr());
            assert!(delivery.data.is_empty());
        }
        assert_eq!(
            delivery.retry(target.as_mut_ptr(), target.len()).unwrap(),
            0
        );
        let calls = CALLS.with(|c| c.replace(None).unwrap());
        assert_eq!(calls, 0);
    }
    assert_eq!(target.as_slice(), source.as_slice());
}
#[test]
fn retained_messages_share_storage_after_parser_and_parent_disposal() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let mut writer = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(Some(1024))
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let channel = writer.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
        for sequence in 0..3 {
            writer
                .write_to_known_channel(
                    &records::MessageHeader {
                        channel_id: channel,
                        sequence,
                        log_time: sequence as u64,
                        publish_time: 0,
                    },
                    &vec![sequence as u8; 70000],
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let input = writer.into_inner().into_inner();
        unsafe {
            let mut reader = ptr::null_mut();
            let mut result = Response::default();
            assert_eq!(
                buffer_reader::fm_buffer_reader_open(
                    5,
                    false,
                    input.as_ptr(),
                    input.len(),
                    &mut reader,
                    &mut result
                ),
                0
            );
            let mut header = MessageHeader::default();
            assert_eq!(
                buffer_reader::fm_buffer_reader_message(
                    reader,
                    ptr::null_mut(),
                    0,
                    &mut header,
                    &mut result
                ),
                2
            );
            let pending = (*reader).delivery.bytes().as_ptr().add(22);
            let mut batch = ptr::null_mut();
            let mut progress = batch::Progress::default();
            assert_eq!(
                lease::fm_read_lease(
                    1,
                    reader.cast(),
                    1,
                    70000,
                    &mut batch,
                    &mut progress,
                    &mut result
                ),
                0
            );
            assert_eq!((&(*batch).messages)[0].data.as_ref().as_ptr(), pending);
            let mut retained = ptr::null_mut();
            assert_eq!(
                lease::fm_lease_retain(batch, 0, &mut retained, &mut result),
                0
            );
            lease::fm_lease_free(batch);
            buffer_reader::fm_buffer_reader_free(reader);
            assert_eq!((&(*retained).messages)[0].data.as_ref().as_ptr(), pending);
            assert_eq!((&(*retained).messages)[0].data.as_ref(), vec![0; 70000]);
            lease::fm_lease_free(retained);
        }
    }
}
#[test]
fn error_formatting_panics_stay_inside_ffi() {
    #[derive(Debug)]
    struct BadDisplay;
    impl std::fmt::Display for BadDisplay {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("display");
        }
    }
    impl std::error::Error for BadDisplay {}
    let mut response = Response::default();
    assert_eq!(guard(&mut response, |_| Err(Box::new(BadDisplay))), -1);
    unsafe {
        assert_eq!(
            bytes(response.json, response.json_len).unwrap(),
            b"Native MCAP panic; operation failed"
        );
        fm_buffer_free(response.json, response.json_len);
    }
}

#[test]
fn complete_request_reservation_avoids_short_read_reallocation() {
    struct ShortInput<'a> {
        remaining: &'a [u8],
        quantum: usize,
    }
    impl Read for ShortInput<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            assert!(out.len() <= 65536);
            let n = out.len().min(self.quantum).min(self.remaining.len());
            out[..n].copy_from_slice(&self.remaining[..n]);
            self.remaining = &self.remaining[n..];
            Ok(n)
        }
    }
    for mib in [1, 8, 32] {
        let size = mib * 1024 * 1024;
        let mut record = vec![42; size + 9];
        record[0] = 0x80;
        record[1..9].copy_from_slice(&(size as u64).to_le_bytes());
        for legacy in [true, false] {
            let mut parser = sans_io::LinearReader::new_with_options(
                sans_io::LinearReaderOptions::default()
                    .with_skip_start_magic(true)
                    .with_skip_end_magic(true),
            );
            let mut source = ShortInput {
                remaining: &record,
                quantum: 65536,
            };
            let measure = Measurement::start(false);
            let started = std::time::Instant::now();
            let mut requests = 0;
            let mut after_reserve = None;
            loop {
                match parser.next_event().unwrap().unwrap() {
                    sans_io::LinearReadEvent::ReadRequest(n) => {
                        if legacy {
                            let count = source
                                .read(parser.try_insert(n.min(65536)).unwrap())
                                .unwrap();
                            parser.notify_read(count);
                        } else {
                            io::feed_linear(&mut source, &mut parser, n).unwrap();
                        }
                        requests += 1;
                        // The record prefix is requested first, then the complete body.
                        if requests == 2 {
                            after_reserve = Some(measure.totals());
                        }
                        if !legacy && requests > 2 {
                            assert_eq!(measure.totals(), after_reserve.unwrap());
                        }
                    }
                    sans_io::LinearReadEvent::Record { opcode, data } => {
                        assert_eq!(opcode, 0x80);
                        assert_eq!(data, &record[9..]);
                        break;
                    }
                }
            }
            let elapsed = started.elapsed();
            let (calls, bytes) = measure.totals();
            drop(measure);
            println!("input-reservation mib={mib} legacy={legacy} requests={requests} allocations={calls} allocated_bytes={bytes} elapsed_us={}", elapsed.as_micros());
        }
    }
}

#[test]
fn bounded_lease_windows_share_storage_and_release_owners() {
    use std::collections::VecDeque;
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let mut writer = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(Some(32768))
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let channel = writer.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
        let payload = [42; 4096];
        for sequence in 0..2048 {
            writer
                .write_to_known_channel(
                    &records::MessageHeader {
                        channel_id: channel,
                        sequence,
                        log_time: sequence as u64,
                        publish_time: 0,
                    },
                    &payload,
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let file = writer.into_inner().into_inner();
        for mapped in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "mcap-storage-{}-{compression:?}-{mapped}.mcap",
                std::process::id()
            ));
            if mapped {
                std::fs::write(&path, &file).unwrap();
            }
            for window in [1, 4, 16] {
                unsafe {
                    let mut reader = ptr::null_mut();
                    let mut result = Response::default();
                    if mapped {
                        let config = serde_json::to_vec(&json!({"path": path, "mode": 5})).unwrap();
                        assert_eq!(
                            buffer_reader::fm_buffer_reader_mapped(
                                config.as_ptr(),
                                config.len(),
                                &mut reader,
                                &mut result
                            ),
                            0
                        );
                    } else {
                        assert_eq!(
                            buffer_reader::fm_buffer_reader_open(
                                5,
                                false,
                                file.as_ptr(),
                                file.len(),
                                &mut reader,
                                &mut result
                            ),
                            0
                        );
                    }
                    let input = Arc::downgrade(&(*reader).input);
                    let input_start = (*reader).input.as_ptr() as usize;
                    let input_length = (&(*reader).input).len();
                    let mut queue = VecDeque::with_capacity(window + 1);
                    let measure = Measurement::start(true);
                    let started = std::time::Instant::now();
                    let mut batches = 0;
                    let mut middle_peak = 0;
                    let mut tail_peak = 0;
                    let mut max_owners = 0;
                    let mut last_owners = [(0usize, 0usize); 64];
                    let mut last_count = 0;
                    loop {
                        let mut batch = ptr::null_mut();
                        let mut progress = batch::Progress::default();
                        let status = lease::fm_read_lease(
                            1,
                            reader.cast(),
                            4,
                            16384,
                            &mut batch,
                            &mut progress,
                            &mut result,
                        );
                        assert!(status >= 0);
                        if batch.is_null() {
                            break;
                        }
                        assert_eq!(progress.count, 4);
                        assert_eq!(progress.bytes, 16384);
                        batches += 1;
                        queue.push_back(batch);
                        if queue.len() > window {
                            lease::fm_lease_free(queue.pop_front().unwrap());
                        }
                        let mut owners = [(0usize, 0usize); 64];
                        let mut count = 0;
                        for &b in &queue {
                            for message in &(*b).messages {
                                let data = message.data.as_ref();
                                let pointer = data.as_ptr() as usize;
                                let owner = if pointer >= input_start
                                    && pointer + data.len() <= input_start + input_length
                                {
                                    (input_start, if mapped { 0 } else { input_length })
                                } else {
                                    measure.owner(data)
                                };
                                if !owners[..count].contains(&owner) {
                                    owners[count] = owner;
                                    count += 1;
                                }
                                assert_eq!(data, &payload);
                            }
                        }
                        let capacity = owners[..count].iter().map(|o| o.1).sum::<usize>();
                        if compression.is_some() {
                            assert!(capacity <= (window + 1) * 65536);
                        }
                        if (128..256).contains(&batches) {
                            middle_peak = middle_peak.max(capacity);
                        }
                        if batches >= 384 {
                            tail_peak = tail_peak.max(capacity);
                        }
                        max_owners = max_owners.max(count);
                        last_owners = owners;
                        last_count = count;
                    }
                    assert_eq!(batches, 512);
                    assert_eq!(middle_peak, tail_peak);
                    // Retaining a message must not change its address or copy its storage.
                    let parent = *queue.back().unwrap();
                    let expected = (&(*parent).messages)[0].data.as_ref().as_ptr();
                    let mut retained = ptr::null_mut();
                    assert_eq!(
                        lease::fm_lease_retain(parent, 0, &mut retained, &mut result),
                        0
                    );
                    buffer_reader::fm_buffer_reader_free(reader);
                    for batch in queue.drain(..) {
                        lease::fm_lease_free(batch);
                    }
                    assert_eq!((&(*retained).messages)[0].data.as_ref().as_ptr(), expected);
                    assert_eq!((&(*retained).messages)[0].data.as_ref(), &payload);
                    lease::fm_lease_free(retained);
                    assert!(input.upgrade().is_none());
                    LIVE.with(|v| {
                        let v = v.borrow();
                        for (p, _) in &last_owners[..last_count] {
                            assert!(!v.as_ref().unwrap().iter().any(|(live, _)| live == p));
                        }
                    });
                    let (allocations, allocated_bytes) = measure.totals();
                    let elapsed = started.elapsed();
                    drop(measure);
                    println!("lease-storage compression={compression:?} mapped={mapped} window={window} batches={batches} messages_per_batch=4 logical_retained={} unique_owners={max_owners} middle_capacity={middle_peak} tail_capacity={tail_peak} mapped_address_bytes={} allocations={allocations} allocated_bytes={allocated_bytes} elapsed_us={}", window * 16384, if mapped { input_length } else { 0 }, elapsed.as_micros());
                }
            }
            if mapped {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}

#[test]
fn sort_compaction_profile() {
    // Same source sizes and selections for both policies. This isolates binding sort storage;
    // codec costs and process RSS are not measured by this fixture.
    for selected in [4096usize, 1024 * 1024] {
        for baseline in [true, false] {
            let measure = Measurement::start(true);
            let start = std::time::Instant::now();
            let mut arena = sort_arena::Arena::default();
            arena.shared_baseline = baseline;
            let mut first_owner = None;
            for i in 0..16 {
                let mut parser = sans_io::LinearReader::new_with_options(
                    sans_io::LinearReaderOptions::default()
                        .with_skip_start_magic(true)
                        .with_skip_end_magic(true),
                );
                let size = 1024 * 1024;
                let dest = parser.try_insert(size + 9).unwrap();
                dest[0] = 0x80;
                dest[1..9].copy_from_slice(&(size as u64).to_le_bytes());
                dest[9..].fill(42);
                parser.notify_read(size + 9);
                let Some(Ok(sans_io::linear_reader::SharedReadEvent::Record { data, .. })) =
                    parser.next_shared_event()
                else {
                    panic!("record")
                };
                if i == 0 {
                    first_owner = Some(measure.owner(data.as_ref()).0);
                }
                arena
                    .push_shared(
                        MessageHeader {
                            sequence: i,
                            log_time: 16 - i as u64,
                            ..Default::default()
                        },
                        data.slice(0..selected),
                        &memory::Options::default(),
                    )
                    .unwrap();
                // Check immediately after group closure, before later allocations can reuse
                // the released address (address reuse alone is not an ownership leak).
                if i == 1 && !baseline && selected == 4096 {
                    LIVE.with(|v| {
                        assert!(!v
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .iter()
                            .any(|(p, _)| Some(*p) == first_owner))
                    });
                }
            }
            arena.sort(false).unwrap();
            let ready_us = start.elapsed().as_micros();
            let copied = arena.copied_bytes;
            let overlap = arena.peak_compaction_overlap;
            let compact = !baseline && selected == 4096;
            assert_eq!(copied, if compact { selected * 16 } else { 0 });
            let mut owners = [(0usize, 0usize); 16];
            let mut count = 0;
            let mut retained = Vec::new();
            let mut header = MessageHeader::default();
            let mut response = Response::default();
            while let Some(data) = arena.read_shared(&mut header, &mut response) {
                assert_eq!(header.sequence as usize, 15 - retained.len());
                let owner = measure.owner(data.as_ref());
                if !owners[..count].contains(&owner) {
                    owners[count] = owner;
                    count += 1;
                }
                retained.push(data);
            }
            let capacity = owners[..count].iter().map(|(_, n)| n).sum::<usize>();
            assert_eq!(
                capacity,
                if compact {
                    selected * 16
                } else {
                    (1024 * 1024 + 9) * 16
                }
            );
            let elapsed = start.elapsed();
            let (allocations, allocated_bytes) = measure.totals();
            drop(arena);
            assert!(retained
                .iter()
                .all(|data| data.as_ref().iter().all(|b| *b == 42)));
            drop(retained);
            LIVE.with(|v| {
                assert!(owners[..count].iter().all(|(p, _)| !v
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|(live, _)| live == p)))
            });
            drop(measure);
            println!("sort-storage baseline={baseline} selected_per_owner={selected} owners={count} retained_capacity={capacity} copied_bytes={copied} peak_group_old_plus_new={overlap} allocations={allocations} allocated_bytes={allocated_bytes} first_result_us={ready_us} total_us={} messages_per_second={}", elapsed.as_micros(), 16.0 / elapsed.as_secs_f64());
        }
    }
}

#[test]
fn random_access_profile() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let mut writer = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(None)
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let channel = writer.add_channel(0, "t", "raw", &BTreeMap::new()).unwrap();
        for chunk in 0..4 {
            for message in 0..16 {
                let sequence = chunk * 16 + message;
                writer
                    .write_to_known_channel(
                        &records::MessageHeader {
                            channel_id: channel,
                            sequence,
                            log_time: sequence as u64,
                            publish_time: 0,
                        },
                        &[42; 4096],
                    )
                    .unwrap();
            }
            writer.flush().unwrap();
        }
        writer.finish().unwrap();
        let input = Arc::new(memory::Backing::Owned {
            data: writer.into_inner().into_inner(),
        });
        let summary = Arc::new(mcap::Summary::read(&input).unwrap().unwrap());
        assert_eq!(summary.chunk_indexes.len(), 4);
        let entries: Vec<_> = summary
            .chunk_indexes
            .iter()
            .map(|index| {
                summary
                    .read_message_indexes(&input, index)
                    .unwrap()
                    .into_values()
                    .next()
                    .unwrap()
            })
            .collect();
        // Prepared, distinct keys isolate cache behavior from caller index encoding.
        let keys: Vec<_> = summary
            .chunk_indexes
            .iter()
            .map(|index| index.chunk_start_offset.to_le_bytes())
            .collect();
        for limit in [0u64, 100_000, 1_000_000] {
            let mut cache = chunk_cache::ChunkCache::new();
            let measure = Measurement::start(true);
            let start = std::time::Instant::now();
            let mut latencies = [0u128; 192];
            let mut n = 0;
            for _ in 0..3 {
                for (i, index) in summary.chunk_indexes.iter().enumerate() {
                    for entry in &entries[i] {
                        let tick = std::time::Instant::now();
                        let message = cache
                            .message(&input, &summary, index, &keys[i], entry.offset, limit)
                            .unwrap();
                        assert_eq!(message.data.as_ref(), &[42; 4096]);
                        latencies[n] = tick.elapsed().as_nanos();
                        n += 1;
                    }
                }
            }
            assert_eq!(
                cache.loads,
                match limit {
                    0 => 192,
                    100_000 => 12,
                    _ => 4,
                }
            );
            assert_eq!(cache.hits + cache.loads, 192);
            let elapsed = start.elapsed();
            let (calls, bytes) = measure.totals();
            let (charge, payloads) = cache.diagnostic_storage();
            let mut owners = Vec::new();
            for payload in payloads {
                let pointer = payload.as_ref().as_ptr() as usize;
                let input_start = input.as_ptr() as usize;
                let owner = if pointer >= input_start
                    && pointer + payload.as_ref().len() <= input_start + input.len()
                {
                    (input_start, input.capacity())
                } else {
                    measure.owner(payload.as_ref())
                };
                if !owners.contains(&owner) {
                    owners.push(owner);
                }
            }
            let retained: usize = owners.iter().map(|(_, n)| n).sum();
            latencies.sort_unstable();
            let loads = cache.loads;
            let hits = cache.hits;
            drop(cache);
            drop(measure);
            println!("random-access compression={compression:?} cache_limit={limit} chunk_loads={} cache_hits={} cache_charge_bytes={charge} unique_backing_capacity={retained} input_capacity={} allocations={calls} allocated_bytes={bytes} elapsed_us={} median_ns={} p95_ns={}", loads, hits, input.capacity(), elapsed.as_micros(), latencies[96], latencies[182]);
        }
        // Read the same messages in chunk order through the binding's lazy chunk cursor.
        let measure = Measurement::start(false);
        let start = std::time::Instant::now();
        let mut delivered = 0;
        for _ in 0..3 {
            for index in &summary.chunk_indexes {
                let cursor =
                    buffer_reader::chunk_reader(input.clone(), summary.clone(), index).unwrap();
                let mut payload = [0u8; 4096];
                let mut result = Response::default();
                let mut header = MessageHeader::default();
                // Use the ABI to exercise the same cursor as OpenChunkReader.
                let handle = Box::into_raw(Box::new(cursor));
                unsafe {
                    loop {
                        let status = buffer_reader::fm_buffer_reader_message(
                            handle,
                            payload.as_mut_ptr(),
                            payload.len(),
                            &mut header,
                            &mut result,
                        );
                        if status == 1 {
                            break;
                        }
                        assert_eq!(status, 0);
                        assert_eq!(payload, [42; 4096]);
                        delivered += 1;
                    }
                    buffer_reader::fm_buffer_reader_free(handle);
                }
            }
        }
        let elapsed = start.elapsed();
        let (calls, bytes) = measure.totals();
        drop(measure);
        assert_eq!(delivered, 192);
        println!("chunk-sequential compression={compression:?} chunk_cursors=12 messages={delivered} cache_retained_bytes=0 allocations={calls} allocated_bytes={bytes} elapsed_us={}", elapsed.as_micros());
    }
}
