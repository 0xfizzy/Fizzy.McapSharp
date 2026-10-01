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
                    v.swap_remove(i);
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
