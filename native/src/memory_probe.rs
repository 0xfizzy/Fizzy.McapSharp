//! Test-only evidence for binding delivery, not a production memory ledger.
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
thread_local! { static CALLS: Cell<Option<usize>> = const { Cell::new(None) }; }
struct Probe;
#[global_allocator]
static ALLOCATOR: Probe = Probe;
fn count() {
    let _ = CALLS.try_with(|c| {
        if let Some(n) = c.get() {
            c.set(Some(n + 1));
        }
    });
}
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        count();
        System.realloc(p, layout, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
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
