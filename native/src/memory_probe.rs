//! Test-only allocator observations, isolated to the measuring thread.
use super::*;
use std::borrow::Cow;
#[path = "allocation_audit.rs"]
mod identity;
use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64,Ordering};
static LIVE: AtomicU64=AtomicU64::new(0);
static PEAK: AtomicU64=AtomicU64::new(0);
fn allocated(n:usize) { let live=LIVE.fetch_add(n as u64,Ordering::Relaxed)+n as u64; PEAK.fetch_max(live,Ordering::Relaxed); }

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
        let p=identity::alloc(l,false); if !p.is_null() {allocated(l.size());} p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count(l.size());
        let p=identity::alloc(l,true); if !p.is_null() {allocated(l.size());} p
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count(n);
        let result=identity::realloc(p,l,n);
        if !result.is_null() { LIVE.fetch_sub(l.size() as u64,Ordering::Relaxed); allocated(n); }
        result
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as u64,Ordering::Relaxed);
        identity::dealloc(p, l)
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
            eprintln!("Rust allocator process live={} peak={} (includes budgeted codec callbacks; direct foreign allocations excluded)",LIVE.load(Ordering::Relaxed),PEAK.load(Ordering::Relaxed));
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
            eprintln!("Rust allocator process live={} peak={} (includes budgeted codec callbacks; direct foreign allocations excluded)",LIVE.load(Ordering::Relaxed),PEAK.load(Ordering::Relaxed));
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
        let key = buffer_reader::encode_chunk(index)
            .unwrap()
            .1;
        for cached in [false, true] {
            let mut cache = chunk_cache::ChunkCache::default();
            let options=memory::Options::default();
            let shared=memory::Source::new(memory::Backing::copy(&data,options.clone()).unwrap(), &options.domain).unwrap();
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
                                    &shared,
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
            eprintln!("Rust allocator process live={} peak={} (includes budgeted codec callbacks; direct foreign allocations excluded)",LIVE.load(Ordering::Relaxed),PEAK.load(Ordering::Relaxed));
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

#[test]
fn prepared_index_cached_calls_allocate_nothing() {
    for compression in [None, Some(mcap::Compression::Lz4), Some(mcap::Compression::Zstd)] {
        let mut writer = mcap::WriteOptions::new().compression(compression).chunk_size(None)
            .create(std::io::Cursor::new(Vec::new())).unwrap();
        for i in 0..110 {
            let channel = writer.add_channel(0, &format!("t{i}"), "raw", &BTreeMap::new()).unwrap();
            writer.write_to_known_channel(&records::MessageHeader { channel_id: channel, sequence: i, log_time: i as u64, publish_time: 0 }, &[42; 64]).unwrap();
        }
        writer.finish().unwrap();
        let data = writer.into_inner().into_inner();
        let summary = mcap::Summary::read(&data).unwrap().unwrap();
        let index = &summary.chunk_indexes[0];
        let indexes = summary.read_message_indexes(&data, index).unwrap();
        let entry = indexes.values().next().unwrap()[0].clone();
        let key = buffer_reader::encode_chunk(index).unwrap().1;
        assert!(key.len() > 1024);
        unsafe {
            let mut snapshot = ptr::null_mut(); let mut prepared = ptr::null_mut(); let mut r = Response::default();
            let config = br#"{"MaxRandomAccessCacheBytes":1048576}"#;
            assert_eq!(extended::fm_snapshot_bytes_options(data.as_ptr(), data.len(), config.as_ptr(), config.len(), &mut snapshot, &mut r), 0);
            assert_eq!(extended::fm_chunk_index_prepare(key.as_ptr(), key.len(), &mut prepared, &mut r), 0);
            let mut output = [0; 64]; let mut header = MessageHeader::default();
            assert_eq!(extended::fm_snapshot_prepared_call(snapshot, 2, prepared, entry.log_time, entry.offset, output.as_mut_ptr(), output.len(), &mut header, &mut r), 0);
            COUNTS.with(|c| c.set(Some((0, 0))));
            for _ in 0..100 {
                assert_eq!(extended::fm_snapshot_prepared_call(snapshot, 2, prepared, entry.log_time, entry.offset, output.as_mut_ptr(), output.len(), &mut header, &mut r), 0);
            }
            let counts = COUNTS.with(|c| c.replace(None).unwrap());
            eprintln!("Rust allocator process live={} peak={} (includes budgeted codec callbacks; direct foreign allocations excluded)",LIVE.load(Ordering::Relaxed),PEAK.load(Ordering::Relaxed));
            assert_eq!(counts, (0, 0), "prepared cached seeks {compression:?}");
            extended::fm_chunk_index_free(prepared); extended::fm_snapshot_free(snapshot);
        }
    }
}

#[test]
fn controlled_allocation_identities_match_independent_allocator() {
    decoder_failure_transport_has_no_unattributed_allocations();
    encoder_failure_transport_has_no_unattributed_allocations();
    buffered_writer_output_failures_have_exact_allocation_identities();
    wait_failure_construction_and_encoding_allocate_nothing();
    response_buffers_and_streamed_descriptions_have_exact_identities();
    streamed_summary_has_one_exact_response_allocation();
    random_record_input_has_exact_allocation_identities();
    string_map_validation_has_exact_allocation_identities();
    borrowed_record_validation_has_no_heap_allocations();
    metadata_duplicate_error_has_no_unaccounted_allocations();
    real_budget_rejection_in_string_maps_has_no_error_allocations();
    real_budget_rejection_in_declarations_has_no_error_allocations();
    real_budget_rejection_in_shared_indexes_has_no_error_allocations();
    fixed_shared_controls_have_no_error_allocations();
    fixed_registry_refusals_have_no_error_allocations();
    borrowed_chunk_headers_have_no_heap_allocations();
    indexed_bodies_have_charged_allocations_without_payload_copies();
    configured_json_uses_selected_budget_before_allocation();
    writer_option_text_has_exact_shared_allocation_identity();
    writer_constructor_control_and_text_are_fully_charged();
    indexed_channel_filter_pages_have_exact_allocation_identity();
    indexed_constructor_capacity_errors_do_not_allocate();
    fixed_reservation_errors_preserve_classification_without_allocations();
    linear_input_capacity_errors_have_exact_allocation_identities();
    retained_topic_filters_have_exact_allocation_identities();
    copied_buffer_constructor_has_exact_allocation_identities();
    codec_callback_failures_do_not_allocate_error_payloads();
    summary_control_has_exact_identity_and_shared_purpose_pins();
    domain_root_bootstrap_has_exact_allocation_identity();
    budget_registry_controls_have_exact_identity_and_zero_allocation_notifications();
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer = mcap::WriteOptions::new().use_chunks(false).memory_budget(domain.clone())
            .create(memory::Measure::default()).unwrap();
        let baseline_live = domain.workload_detailed_statistics().resources.map(|r| r.live);
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        for _ in 0..1000 { writer.attach_borrowed(1,2,"name","raw",&[1,2,3]).unwrap(); }
        let summary = writer.finish().unwrap(); let retained=summary.clone();
        let calls=COUNTS.with(|c| c.replace(None).unwrap()); let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes),"attachment names or completion allocated outside the budget");
        assert_eq!(actual.errors,0); assert!(actual.bound>=3000);
        drop(summary); writer.into_inner();
        assert_eq!(retained.attachment_indexes.len(),1000); assert_eq!(retained.attachment_indexes[999].data_size,3);
        let live=domain.workload_detailed_statistics().resources.map(|r|r.live);
        assert_eq!(identity::snapshot().live,std::array::from_fn(|i|live[i]-baseline_live[i]));
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Operation as usize],domain.workload_statistics().current);
        drop(retained); assert_eq!(identity::snapshot().live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0); assert_eq!(domain.ownership_statistics(),Default::default());
        eprintln!("retained attachment/complete identity audit: matched={}, unattributed=0 (writer/options bootstrap excluded)",actual.bound);
        drop(audit);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut writer = mcap::WriteOptions::new().use_chunks(false).memory_budget(domain.clone())
            .create(memory::Measure::default()).unwrap();
        let baseline_live = domain.workload_detailed_statistics().resources.map(|r| r.live);
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        for _ in 0..1000 { writer.write_metadata_borrowed("name", std::iter::empty()).unwrap(); }
        let summary = writer.finish().unwrap();
        let retained = summary.clone();
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(calls, (actual.bound, actual.bound_bytes), "metadata retention or summary cloning allocated outside the budget");
        assert!(actual.bound >= 2000);
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Operation as usize], domain.workload_statistics().current);
        drop(summary);
        writer.into_inner();
        assert_eq!(retained.metadata_indexes.len(), 1000);
        assert_eq!(retained.metadata_indexes[999].name, "name");
        let live = domain.workload_detailed_statistics().resources.map(|r| r.live);
        assert_eq!(identity::snapshot().live, std::array::from_fn(|i| live[i] - baseline_live[i]));
        drop(retained);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        eprintln!("retained metadata/complete identity audit: matched={}, unattributed=0 (writer/options bootstrap excluded)", actual.bound);
        drop(audit);
    }
    {
        let mut wire = [0u8; 46 + 30];
        wire[42..46].copy_from_slice(&30u32.to_le_bytes());
        for (i, key) in [0u16,256,65535].into_iter().enumerate() {
            wire[46+i*10..48+i*10].copy_from_slice(&key.to_le_bytes());
            wire[48+i*10..56+i*10].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        }
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let value = mcap::shared_statistics::SharedStatistics::read(
            &mut std::io::Cursor::new(&wire), domain.clone(), mcap::storage::OwnerKind::Parser,
        ).unwrap();
        let retained = value.clone();
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.bound, 4);
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        assert_eq!(actual.errors, 0);
        drop(value);
        assert_eq!(retained.channel_message_counts[&65535], 3);
        drop(retained);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        eprintln!("shared statistics identity audit: matched=4, unattributed=0");
        drop(audit);
    }
    for count in [3usize, 5] {
        let mut wire = [0u8; 122];
        wire[32..36].copy_from_slice(&((count * 10) as u32).to_le_bytes());
        for (i, key) in [0u16,256,65535,255,512].into_iter().take(count).enumerate() {
            wire[36+i*10..38+i*10].copy_from_slice(&key.to_le_bytes());
            wire[38+i*10..46+i*10].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        }
        let string_start = 48 + count * 10;
        wire[string_start-4..string_start].copy_from_slice(&8u32.to_le_bytes());
        wire[string_start..string_start+8].copy_from_slice(b"testcode");
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let value = mcap::shared_chunk_index::SharedChunkIndex::read(
            &wire, &domain, mcap::storage::OwnerKind::Parser,
        ).unwrap();
        let retained = value.clone();
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.bound, if count == 3 { 2 } else { 7 });
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        assert_eq!(actual.errors, 0);
        drop(value);
        assert_eq!(retained.message_index_offsets[&65535], 3);
        drop(retained);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        eprintln!("shared chunk index/{count} identity audit: matched={}, unattributed=0", actual.bound);
        drop(audit);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let prepared = extended::PreparedChunkIndex::new(&wire, &domain).unwrap();
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.bound, if count == 3 { 4 } else { 9 });
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        assert_eq!(actual.errors, 0);
        drop(prepared);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        eprintln!("prepared chunk index/{count} identity audit: matched={}, unattributed=0", actual.bound);
        drop(audit);
        for failure in 0..actual.bound {
            let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failure as usize);
            assert!(extended::PreparedChunkIndex::new(&wire, &domain).is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let options = mcap::WriteOptions::new().memory_budget(domain.clone()).use_chunks(false);
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let writer = options.create(memory::Measure::default()).unwrap();
        let calls = COUNTS.with(|c| c.replace(None)).unwrap();
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 1);
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r|r.live));
        writer.into_inner();
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
        eprintln!("writer initialization identity audit: matched=1, unattributed=0 (options input excluded)");
    }

    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let channel = extended::PreparedChannel::new(br#"{"id":9,"topic":"topic","encoding":"raw","metadata":{"a":"value"},"schema":{"id":7,"name":"schema","encoding":"raw"}}"#, &[42;4096], &domain).unwrap();
        let calls = COUNTS.with(|c| c.replace(None)).unwrap();
        let actual = identity::snapshot();
        let charged = domain.workload_detailed_statistics();
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.live, charged.resources.map(|r|r.live));
        assert_eq!(actual.bound, charged.allocation_count);
        assert_eq!(domain.workload_statistics().current, actual.live.iter().sum::<u64>());
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Operation as usize], domain.workload_statistics().current);
        drop(channel);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
        eprintln!("prepared channel identity audit: matched={}, unattributed=0", actual.bound);
    }
    {
        // Caller metadata and writer bootstrap predate this audit. Registration
        // itself must have no unclaimed temporary or retained heap allocation.
        let metadata: BTreeMap<_, _> = (0..1000).map(|n| (format!("key{n:04}"), format!("value{n}"))).collect();
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let mut writer = mcap::WriteOptions::new().memory_budget(domain.clone()).use_chunks(false)
            .create(memory::Measure::default()).unwrap();
        let before = domain.workload_detailed_statistics();
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c| c.set(Some((0, 0))));
        let schema = writer.add_schema("audit-schema", "raw", &[42; 4096]).unwrap();
        let channel = writer.add_channel(schema, "audit-channel", "raw", &metadata).unwrap();
        assert_eq!(writer.add_channel(schema, "audit-channel", "raw", &metadata).unwrap(), channel);
        assert_eq!(writer.add_schema("audit-schema", "raw", &[42; 4096]).unwrap(), schema);
        let allocation_calls = COUNTS.with(|c| c.replace(None)).unwrap();
        let actual = identity::snapshot();
        let charged = domain.workload_detailed_statistics();
        assert_eq!(actual.errors, 0);
        // Registration is synchronous and has no codec workers. Thread-local
        // allocator totals exclude concurrent tests; the identity audit below
        // independently proves each claimed pointer/layout, not only totals.
        assert_eq!(allocation_calls, (actual.bound, actual.bound_bytes), "declaration registration contains an uncharged allocation");
        assert_eq!(actual.bound, charged.allocation_count - before.allocation_count);
        assert_eq!(actual.bound_bytes, charged.allocated_bytes - before.allocated_bytes);
        assert_eq!(actual.live, std::array::from_fn(|i| charged.resources[i].live - before.resources[i].live));
        // The shared bookkeeping control and its conservative reservation are
        // also owned by this writer, including its pre-audit control allocation.
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Operation as usize], domain.workload_statistics().current);
        writer.into_inner();
        assert_eq!(identity::snapshot().live, [0; 9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
        eprintln!("declaration registration identity audit: matched={}, unattributed=0", actual.bound);
    }
    for (compression, workers) in [(None,0), (Some(mcap::Compression::Lz4),0),
        (Some(mcap::Compression::Zstd),0), (Some(mcap::Compression::Zstd),1), (Some(mcap::Compression::Zstd),2)] {
        let metadata = BTreeMap::from([("a".to_owned(),"first".to_owned()),("z".to_owned(),"最后".to_owned())]);
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            retained: 0, ..Default::default()
        }).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let mut writer = mcap::WriteOptions::new().memory_budget(domain.clone()).compression(compression)
            .compression_threads(workers).chunk_size(Some(16384)).create(memory::Measure::default()).unwrap();
        let initialization_unattributed = identity::snapshot().unmatched;
        let schema = writer.add_schema("schema", "raw", &[7;4096]).unwrap();
        let channel = writer.add_channel(schema,"audit","raw",&metadata).unwrap();
        let declarations_unattributed = identity::snapshot().unmatched - initialization_unattributed;
        for sequence in 0..8 {
            writer.write_to_known_channel(&records::MessageHeader { channel_id: channel, sequence,
                log_time: sequence as u64, publish_time: 0 }, &[42;8192]).unwrap();
        }
        let before_finish_unattributed = identity::snapshot().unmatched;
        writer.finish().unwrap();
        let finish_unattributed = identity::snapshot().unmatched - before_finish_unattributed;
        // Finish drains codec workers before comparing independent snapshots.
        let actual = identity::snapshot();
        let charged = domain.workload_detailed_statistics();
        assert_eq!(actual.errors, 0, "allocator identities disagree");
        assert_eq!(actual.live, charged.resources.map(|r|r.live));
        assert_eq!(actual.bound, charged.allocation_count);
        assert_eq!(actual.bound_bytes, charged.allocated_bytes);
        drop(writer);
        let actual = identity::snapshot();
        drop(audit);
        assert!(actual.bound > 0);
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        eprintln!("allocation identity audit {compression:?}/{workers}: matched={}, unattributed={} (closure pending)",actual.bound,actual.unmatched);
        eprintln!("unattributed phases {compression:?}/{workers}: initialization={initialization_unattributed}, declarations={declarations_unattributed}, message/chunks={}, finish={finish_unattributed}", before_finish_unattributed - initialization_unattributed - declarations_unattributed);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let key = chunk_cache::CacheKey::new(&domain, &[7; 257]).unwrap();
        let actual = identity::snapshot();
        let charged = domain.workload_detailed_statistics();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 1);
        assert_eq!(actual.bound_bytes, 257);
        assert_eq!(actual.live, charged.resources.map(|r| r.live));
        let key = mcap::charged::ChargedShared::new(key, &domain, mcap::storage::ResourceCategory::Scratch).unwrap();
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 2);
        assert_eq!(actual.bound_bytes, 257 + key.allocation_bytes() as u64);
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        let alias = key.clone();
        drop(key);
        assert_eq!(identity::snapshot().bound, 2);
        drop(alias);
        assert_eq!(identity::snapshot().live, [0; 9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
    }
    {
        struct Candidate;
        impl mcap::storage::Reclaimable for Candidate {
            fn last_access(&self) -> u64 { 0 }
            fn reclaim(&self) -> bool { false }
        }
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut candidates = Vec::with_capacity(130);
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        for _ in 0..130 {
            candidates.push(mcap::charged::weak::BudgetedArc::new(Candidate, &domain, mcap::storage::ResourceCategory::Scratch).unwrap());
        }
        for candidate in &candidates {
            domain.register_reclaimer(candidate.reclaimer()).unwrap();
        }
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 133);
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        drop(candidates);
        domain.prune_reclaimers();
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.live, [0; 9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let value = mcap::charged::weak::BudgetedArc::new(42u64, &domain, mcap::storage::ResourceCategory::Scratch).unwrap();
        let capacity = value.allocation_bytes() as u64;
        let weak = value.downgrade();
        assert_eq!(identity::snapshot().bound, 1);
        assert_eq!(identity::snapshot().bound_bytes, capacity);
        drop(value);
        assert!(weak.upgrade().is_none());
        assert_eq!(identity::snapshot().live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        assert_eq!(domain.workload_statistics().current, capacity);
        drop(weak);
        assert_eq!(identity::snapshot().errors, 0);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let batch = lease::Batch::new(domain.clone(), 1).unwrap();
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        eprintln!("lease root identity audit: matched={}, unattributed={} (run alone)", actual.bound, actual.unmatched);
        assert_eq!(actual.bound, 4); // Batch root, radix branch, leaf control, descriptor storage.
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        let handle = lease::publish(batch);
        assert_eq!(identity::snapshot().bound, actual.bound);
        unsafe { lease::fm_lease_free(handle); }
        assert_eq!(identity::snapshot().errors, 0);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        drop(audit);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        for _ in 0..10000 {
            let empty = mcap::storage::SharedBytes::empty();
            let lease = empty.clone_for(mcap::storage::OwnerKind::Lease);
            std::hint::black_box(lease.slice(0..0));
        }
        let actual = identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 0);
        assert_eq!(actual.live, [0;9]);
        eprintln!("empty storage identity audit: matched={}, unattributed={} (run alone)", actual.bound, actual.unmatched);
    }
    {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut delivery = memory::Delivery::default();
        delivery.options.domain = domain.clone();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        delivery.reserve(128).unwrap();
        delivery.data.resize(128, 42);
        let shared = delivery.take_shared(4).unwrap();
        let actual = identity::snapshot();
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.bound, 2);
        assert_eq!(actual.live, domain.workload_detailed_statistics().resources.map(|r| r.live));
        let lease = shared.clone_for(mcap::storage::OwnerKind::Lease);
        drop(shared);
        assert_eq!(lease.as_ref(), &[42;124]);
        drop(lease);
        let final_stats = identity::snapshot();
        drop(audit);
        assert_eq!(final_stats.bound, 2);
        assert_eq!(final_stats.errors, 0);
        assert_eq!(final_stats.live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        eprintln!("delivery source identity audit: matched={}, unattributed={} (run alone)", final_stats.bound, final_stats.unmatched);
    }
    {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let options=memory::Options { domain:domain.clone(), ..Default::default() };
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let source=memory::Source::new(memory::Backing::copy(&[7;128],options).unwrap(),&domain).unwrap();
        let lease=source.shared(4..8).clone_for(mcap::storage::OwnerKind::Lease);
        drop(source);
        assert_eq!(lease.as_ref(),&[7;4]);
        let actual=identity::snapshot();
        assert_eq!(actual.bound,2);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        drop(lease);
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        eprintln!("input source identity audit: matched={}, unattributed={} (run alone)",actual.bound,actual.unmatched);
    }
    {
        let path=std::env::temp_dir().join(format!("fizzy-map-identity-{}.bin",std::process::id()));
        std::fs::write(&path,[8;128]).unwrap();
        // OS file/mapping creation is outside the controlled heap probe interval.
        let file=File::open(&path).unwrap();
        let map=unsafe { Mmap::map(&file).unwrap() };
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let owner=io::MappingOwner::new(map,file,&domain).unwrap();
        let backing=memory::Backing::Mapped { mapping:owner.into_shared() };
        let source=memory::Source::new(backing,&domain).unwrap();
        let lease=source.shared(4..8).clone_for(mcap::storage::OwnerKind::Lease);
        drop(source);
        let actual=identity::snapshot();
        assert_eq!(actual.bound,2);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        assert_eq!(domain.mapped_logical_bytes(),128);
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],0);
        assert_eq!(lease.as_ref(),&[8;4]);
        drop(lease);
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.mapped_logical_bytes(),0);
        std::fs::remove_file(path).unwrap();
        eprintln!("mapping controls identity audit: matched={}, unattributed={} (run alone)",actual.bound,actual.unmatched);
    }
    {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let cursor=buffer_reader::BufferReader::empty(memory::Options { domain:domain.clone(), ..Default::default() });
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let handle=cursor.into_handle().unwrap();
        let actual=identity::snapshot();
        assert_eq!(actual.bound,1);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        unsafe { buffer_reader::fm_buffer_reader_free(handle); }
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        eprintln!("cursor root identity audit: matched={}, unattributed={} (run alone)",actual.bound,actual.unmatched);
    }
    {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let writer=mcap::charged::ChargedBox::new(Writer {
            domain:domain.clone(),inner:None,completed_output:None,failed:false,
            recoverable_errors:31,attachment:None,native_summary:None,
        },&domain,mcap::storage::ResourceCategory::Scratch).unwrap();
        writer.charge_owner(mcap::storage::OwnerKind::Operation,true);
        let actual=identity::snapshot();
        assert_eq!(actual.bound,1);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        unsafe { fm_writer_free(writer.into_raw_value()); }
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        eprintln!("writer root identity audit: matched={}, unattributed={} (run alone)",actual.bound,actual.unmatched);
    }
    for args in [
        br#"{"name":"audit","encoding":"raw"}"#.as_slice(),
        br#"{"name":"\uD83D\uDE80","encoding":"raw","metadata":{"key":"\n\u0000"},"ignored":[{},[],true,null,-0,1e100]}"#.as_slice(),
    ] {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let operation=extended::PreparedOperation::new(1,args,&[42;4096],&domain).unwrap();
        let actual=identity::snapshot();
        assert_eq!(actual.bound,6);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        unsafe { extended::fm_operation_free(operation.into_raw_value()); }
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        eprintln!("prepared operation identity audit: matched={}, unattributed={} (run alone)",actual.bound,actual.unmatched);
    }
    {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let path=std::env::temp_dir().join(format!("mcap-control-json-{}-{}.mcap",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let inner=mcap::WriteOptions::new().use_chunks(false).emit_metadata_indexes(false).memory_budget(domain.clone())
            .create(Output::File(File::create(&path).unwrap())).unwrap();
        let mut writer=Writer {domain:domain.clone(),inner:Some(inner),completed_output:None,failed:false,
            recoverable_errors:31,attachment:None,native_summary:None};
        let before=domain.workload_detailed_statistics();
        let owners=domain.ownership_statistics();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let request=br#"{"name":"meta","metadata":{"key":"value"}}"#;
        let mut response=Response::default();
        let status=unsafe {fm_writer_call(&mut writer,4,request.as_ptr(),request.len(),ptr::null(),0,&mut response)};
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(status,0);
        assert_eq!(actual.bound,4);
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_detailed_statistics().resources.map(|r|(r.current,r.live,r.reserved)),before.resources.map(|r|(r.current,r.live,r.reserved)));
        assert_eq!(domain.ownership_statistics(),owners);
        eprintln!("regular metadata control identity audit: matched={}, unattributed={} (writer initialization/index retention excluded)",actual.bound,actual.unmatched);
        drop(writer);
        assert_eq!(domain.workload_statistics().current,0);
        std::fs::remove_file(path).unwrap();
    }
    for count in [0,1,63,64,65,256,4096,65536] {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c|c.set(Some((0,0))));
        let required=lease::Batch::reservation_bytes(count).unwrap();
        assert_eq!(COUNTS.with(|c|c.replace(None).unwrap()),(0,0));
        let batch=lease::Batch::new(domain.clone(),count).unwrap();
        let actual=identity::snapshot();
        assert_eq!(actual.errors,0);
        assert_eq!(actual.bound_bytes,required as u64);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        assert_eq!(batch.allocation_bytes()+batch.messages.allocated_bytes(),required);
        drop(batch);
        assert_eq!(identity::snapshot().live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        drop(audit);
        let limits=mcap::storage::BudgetLimits {total:(required-1) + mcap::storage::BudgetRef::allocation_size(),block:required-1,retained:0};
        let tight=mcap::storage::BudgetRef::new(limits).unwrap();
        assert!(lease::Batch::new(tight.clone(),count).is_err());
        assert_eq!(tight.workload_statistics().current,0);
        assert_eq!(tight.ownership_statistics(),Default::default());
    }
    {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        COUNTS.with(|c|c.set(Some((0,0))));
        let mut ticket=mcap::storage::CapacityWaitTicket::new(&domain).unwrap();
        let calls=COUNTS.with(|c|c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));
        assert_eq!(actual.bound,1);
        assert_eq!(actual.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
        COUNTS.with(|c|c.set(Some((0,0))));
        for _ in 0..1000 {
            assert_eq!(ticket.arm(4096).unwrap(),mcap::storage::CapacityWaitStatus::Ready);
            assert_eq!(ticket.status(),mcap::storage::CapacityWaitStatus::Ready);
            ticket.cancel();
        }
        assert_eq!(COUNTS.with(|c|c.replace(None).unwrap()),(0,0));
        drop(ticket);
        assert_eq!(identity::snapshot().live,[0;9]);
        assert_eq!(identity::snapshot().errors,0);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        eprintln!("capacity ticket identity audit: matched={},unattributed={}",actual.bound,actual.unmatched);
        drop(audit);
    }
    for compression in [None,Some(mcap::Compression::Lz4),Some(mcap::Compression::Zstd)] {
        // Fixture production and caller-owned input are outside the measured reader domain.
        let mut writer=mcap::WriteOptions::new().compression(compression).chunk_size(Some(16384))
            .create(std::io::Cursor::new(Vec::new())).unwrap();
        let schema=writer.add_schema("schema", "raw", &[7;4096]).unwrap();
        let metadata=(0..1000).map(|i|(format!("key/{i:04}"),format!("value/{i}"))).collect();
        let channel=writer.add_channel(schema,"audit","raw",&metadata).unwrap();
        for sequence in 0..8 {
            writer.write_to_known_channel(&records::MessageHeader { channel_id:channel,sequence,
                log_time:sequence as u64,publish_time:0 }, &[42;8192]).unwrap();
        }
        writer.finish().unwrap();
        let data=writer.into_inner().into_inner();
        let domain=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits { retained:0,..Default::default() }).unwrap();
        let mut reader=sans_io::LinearReader::new_with_options_and_budget(Default::default(), domain.clone());
        let mut schemas=buffer_reader::SchemaTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser);
        let mut channels=buffer_reader::ChannelTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser);
        let delivery=memory::Delivery {options:memory::Options {domain:domain.clone(),..Default::default()},..Default::default()};
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
        let mut position=0;
        while let Some(event)=reader.next_shared_event() {
            match event.unwrap() {
              sans_io::linear_reader::SharedReadEvent::Record {opcode,data} if matches!(opcode,records::op::SCHEMA|records::op::CHANNEL)=>{
                buffer_reader::BufferReader::observe(&mut schemas,&mut channels,opcode,data.as_ref(),&delivery).unwrap();
              }
              sans_io::linear_reader::SharedReadEvent::ReadRequest(need)=>{
                let n=need.min(17).min(data.len()-position);
                reader.try_insert(n).unwrap().copy_from_slice(&data[position..position+n]);
                reader.notify_read(n);
                position+=n;
              }
              _=>{}
            }
        }
        assert_eq!(schemas.get(&schema).unwrap().data.as_ref(), &[7;4096]);
        assert_eq!(channels.get(&channel).unwrap().metadata.len(),1000);
        let actual=identity::snapshot();
        let charged=domain.workload_detailed_statistics();
        assert_eq!(actual.errors,0);
        assert_eq!(actual.live,charged.resources.map(|r|r.live));
        assert_eq!(actual.bound,charged.allocation_count);
        assert_eq!(actual.bound_bytes,charged.allocated_bytes);
        drop((reader,schemas,channels,delivery));
        let actual=identity::snapshot();
        drop(audit);
        assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        eprintln!("reader identity audit {compression:?}: matched={}, unattributed={} (closure pending)",actual.bound,actual.unmatched);
    }

}

fn summary_control_has_exact_identity_and_shared_purpose_pins() {
    use mcap::storage::{BudgetLimits, BudgetRef, OwnerKind};
    let domain = BudgetRef::new(BudgetLimits {retained: 0, ..Default::default()}).unwrap();
    let mut writer = mcap::WriteOptions::new().use_chunks(false).memory_budget(domain.clone())
        .create(memory::Measure::default()).unwrap();
    let schema = writer.add_schema("schema", "raw", &[7;1024]).unwrap();
    writer.add_channel(schema, "topic", "raw", &BTreeMap::new()).unwrap();
    let summary = writer.finish().unwrap();
    writer.into_inner();
    let before = domain.detailed_statistics();
    let before_owners = domain.ownership_statistics();
    domain.observe_allocations(identity::claimed);
    let audit = identity::start(domain.as_ptr() as usize);
    COUNTS.with(|c| c.set(Some((0, 0))));
    let operation = retain_summary(summary, &domain, OwnerKind::Operation).unwrap();
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    let bytes = operation.allocation_bytes() as u64;
    assert_eq!(calls, (1, bytes));
    let actual = identity::snapshot();
    assert_eq!((actual.bound, actual.bound_bytes, actual.errors), (1, bytes, 0));
    assert_eq!(domain.detailed_statistics().resources[7].live, before.resources[7].live + bytes);
    COUNTS.with(|c| c.set(Some((0, 0))));
    let parser = operation.clone_with_owner(OwnerKind::Parser);
    for _ in 0..1000 {
        let alias = parser.clone();
        assert_eq!(alias.schemas.get(&schema).unwrap().data.as_ref(), &[7;1024]);
    }
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(calls, (0, 0));
    assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],
        before_owners.bytes[OwnerKind::Parser as usize] + bytes);
    drop(operation);
    assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Operation as usize],
        before_owners.bytes[OwnerKind::Operation as usize]);
    assert_eq!(parser.channels.len(), 1);
    drop(parser);
    assert_eq!(identity::snapshot().live, [0;9]);
    assert_eq!(identity::snapshot().errors, 0);
    assert_eq!(domain.workload_statistics().current, 0);
    assert_eq!(domain.ownership_statistics(), Default::default());
    drop(audit);
}

fn domain_root_bootstrap_has_exact_allocation_identity() {
    use mcap::storage::{BootstrapError, BudgetLimits, BudgetRef};
    let size = BudgetRef::allocation_size();
    COUNTS.with(|c| c.set(Some((0, 0))));
    let refusal = BudgetRef::new(BudgetLimits {total: size - 1, block: 1, retained: 0});
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    assert!(matches!(refusal, Err(BootstrapError::Limit(_))));
    assert_eq!(calls, (0, 0), "bootstrap capacity refusal must not allocate an error");

    let audit = identity::start(0);
    COUNTS.with(|c| c.set(Some((0, 0))));
    let root = BudgetRef::new_observed(
        BudgetLimits {total: size, block: 1, retained: 0}, identity::bootstrap_claimed,
    ).unwrap();
    root.observe_allocations(identity::claimed);
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(calls, (1, size as u64));
    let actual = identity::snapshot();
    assert_eq!((actual.bound, actual.bound_bytes, actual.errors), (1, size as u64, 0));
    assert_eq!(actual.live, root.detailed_statistics().resources.map(|c| c.live));

    let weak = root.downgrade();
    let weak_copy = weak.clone();
    COUNTS.with(|c| c.set(Some((0, 0))));
    for _ in 0..10_000 {
        let clone = root.clone();
        assert!(clone.ptr_eq(&weak.upgrade().unwrap()));
        assert_eq!(clone.statistics().current, size as u64);
        drop(clone);
    }
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(calls, (0, 0));
    drop(root);
    assert!(weak.upgrade().is_none());
    assert_eq!(identity::snapshot().live[8], size as u64);
    drop(weak);
    assert_eq!(identity::snapshot().live[8], size as u64);
    drop(weak_copy);
    assert_eq!(identity::snapshot().live, [0; 9]);
    assert_eq!(identity::snapshot().errors, 0);
    drop(audit);

    // Include the whole lifetime, not just allocations after an unobserved root.
    // The registry retains weak controls and the idle pool retains payloads.
    struct Cache;
    impl mcap::storage::Reclaimable for Cache {
        fn last_access(&self) -> u64 { 1 }
        fn reclaim(&self) -> bool { false }
    }
    let audit = identity::start(0);
    COUNTS.with(|c| c.set(Some((0, 0))));
    let root = BudgetRef::new_observed(BudgetLimits::default(), identity::bootstrap_claimed).unwrap();
    root.observe_allocations(identity::claimed);
    let observer = root.downgrade();
    let mut buffer = sans_io::LinearReader::new_with_options_and_budget(Default::default(), root.clone());
    buffer.try_insert(4096).unwrap();
    drop(buffer);
    assert!(root.statistics().retained > 4096);
    let cache = mcap::charged::weak::BudgetedArc::new(Cache, &root, mcap::storage::ResourceCategory::Scratch).unwrap();
    let cache_weak = cache.downgrade();
    root.register_reclaimer(cache.reclaimer()).unwrap();
    let calls = COUNTS.with(|c| c.replace(None).unwrap());
    let actual = identity::snapshot();
    assert_eq!(calls, (actual.bound, actual.bound_bytes), "every bootstrap/pool/registry allocation must be claimed");
    assert_eq!(identity::snapshot().live, root.detailed_statistics().resources.map(|c|c.live));
    drop(root);
    assert!(observer.upgrade().is_some());
    drop(cache);
    assert!(observer.upgrade().is_none());
    assert!(cache_weak.upgrade().is_none());
    assert_eq!(identity::snapshot().live[0], 0, "last strong releases idle payloads");
    assert_eq!(identity::snapshot().live[8], (size + mcap::charged::weak::BudgetedArc::<Cache>::allocation_size()) as u64,
        "registry cycle is broken but the detached weak control and domain remain charged");
    drop(cache_weak);
    assert_eq!(identity::snapshot().live[8], size as u64);
    drop(observer);
    assert_eq!(identity::snapshot().live, [0;9]);
    assert_eq!(identity::snapshot().errors, 0);
    drop(audit);
}

#[test]
fn summary_declaration_views_allocate_nothing() {
    let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
    let schema=mcap::shared_declarations::SharedSchema::new(1,"schema","raw",&[42;4096],&domain,mcap::storage::OwnerKind::Parser).unwrap();
    let channel=mcap::shared_declarations::SharedChannel::new(1,"topic","raw",Some(schema.clone()),[("key","value")].into_iter(),&domain,mcap::storage::OwnerKind::Parser).unwrap();
    let mut output = [0u8; 8192];
    COUNTS.with(|c| c.set(Some((0, 0))));
    for _ in 0..1000 {
        for view in [records::SummaryRecordRef::Schema(&schema), records::SummaryRecordRef::Channel(&channel)] {
            view.write_body(&mut std::io::Cursor::new(output.as_mut_slice())).unwrap();
        }
    }
    let counts = COUNTS.with(|c| c.replace(None)).unwrap();
    assert_eq!(counts, (0, 0));
}

#[test]
fn borrowed_metadata_serialization_allocates_nothing_without_retained_indexes() {
    let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
    let doc=budget_json::Document::parse(br#"{"metadata":{"a":"one","z":"two"}}"#,&domain).unwrap();
    let owned=records::Metadata {name:"owned".into(),metadata:BTreeMap::from([("a".into(),"one".into()),("z".into(),"two".into())])};
    let mut output=[0;16384];
    let mut writer=mcap::WriteOptions::new().use_chunks(false).emit_metadata_indexes(false)
        .create(std::io::Cursor::new(output.as_mut_slice())).unwrap();
    COUNTS.with(|c|c.set(Some((0,0))));
    for _ in 0..100 {
        writer.write_metadata_borrowed("borrowed",budget_json::Control::Charged(doc.view().get("metadata")).strings().unwrap()).unwrap();
        writer.write_metadata(&owned).unwrap();
    }
    let counts=COUNTS.with(|c|c.replace(None)).unwrap();
    assert_eq!(counts,(0,0));
}

#[test]
fn attachment_headers_do_not_allocate_temporary_storage() {
    for crc in [false, true] {
        let headers: Vec<_> = (0..100).map(|n| records::AttachmentHeader {
            log_time: n, create_time: 0, name: format!("attachment-{n}"), media_type: "text/plain".into(),
        }).collect();
        let mut output = [0;16384];
        let mut writer = mcap::WriteOptions::new().use_chunks(false).emit_attachment_indexes(false)
            .calculate_attachment_crcs(crc).create(std::io::Cursor::new(output.as_mut_slice())).unwrap();
        COUNTS.with(|c| c.set(Some((0, 0))));
        for header in headers {
            writer.start_attachment(3, header).unwrap();
            writer.put_attachment_bytes(&[1, 2, 3]).unwrap();
            writer.finish_attachment().unwrap();
        }
        let counts = COUNTS.with(|c| c.replace(None)).unwrap();
        assert_eq!(counts, (0, 0));
        writer.finish().unwrap();
    }
}

#[test]
fn default_writer_options_clone_does_not_allocate_unused_storage() {
    COUNTS.with(|counts| counts.set(Some((0, 0))));
    let options = mcap::WriteOptions::new();
    let clone = options.clone();
    let allocations = COUNTS.with(|counts| counts.replace(None).unwrap());
    assert_eq!(allocations, (0, 0));
    drop(clone);
}

#[test]
fn explicit_parser_domains_do_not_allocate_discarded_defaults() {
    let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
    COUNTS.with(|counts| counts.set(Some((0, 0))));
    let linear = sans_io::LinearReader::new_with_options_and_budget(Default::default(), domain.clone());
    let summary = sans_io::SummaryReader::new_with_options_and_budget(Default::default(), domain.clone());
    let allocations = COUNTS.with(|counts| counts.replace(None).unwrap());
    assert_eq!(allocations, (0, 0));
    drop((linear, summary));
}

#[test]
fn reader_declaration_refusals_preserve_retained_tables() {
    let schema_wire=buffer_reader::encode(records::Record::Schema {
        header:records::SchemaHeader {id:513,name:"schema".into(),encoding:"raw".into()},
        data:Cow::Borrowed(&[7;4096]),
    }).unwrap().1;
    let channel_wire=buffer_reader::encode(records::Record::Channel(records::Channel {
        id:65535,schema_id:513,topic:"topic".into(),message_encoding:"raw".into(),
        metadata:(0..300).map(|i|(format!("key/{i:04}"),format!("value/{i}"))).collect(),
    })).unwrap().1;
    for opcode in [records::op::SCHEMA,records::op::CHANNEL] {
        let mut allocation_count=None;
        for failure in std::iter::once(None).chain((0..1024).map(Some)) {
            if failure.is_some_and(|n| n >= allocation_count.unwrap()) {break;}
            let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
            let mut schemas=buffer_reader::SchemaTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser);
            let mut channels=buffer_reader::ChannelTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser);
            let delivery=memory::Delivery {options:memory::Options {domain:domain.clone(),..Default::default()},..Default::default()};
            if opcode==records::op::CHANNEL {
                buffer_reader::BufferReader::observe(&mut schemas,&mut channels,records::op::SCHEMA,&schema_wire,&delivery).unwrap();
            }
            let before=domain.workload_detailed_statistics();
            let owners=domain.ownership_statistics();
            if let Some(failure)=failure {domain.fail_allocation_at(failure);}
            let result=buffer_reader::BufferReader::observe(&mut schemas,&mut channels,opcode,
                if opcode==records::op::SCHEMA {&schema_wire} else {&channel_wire},&delivery);
            if failure.is_some() {
                assert!(result.is_err());
                assert!(channels.is_empty());
                assert_eq!(schemas.len(),if opcode==records::op::CHANNEL {1} else {0});
                if opcode==records::op::CHANNEL {assert_eq!(schemas.get(&513).unwrap().data.as_ref(),&[7;4096]);}
                assert_eq!(domain.workload_statistics().current,before.resources.iter().map(|r|r.current).sum::<u64>());
                assert_eq!(domain.ownership_statistics(),owners);
            } else {
                result.unwrap();
                allocation_count=Some((domain.workload_detailed_statistics().allocation_count-before.allocation_count) as usize);
                assert!(allocation_count.unwrap()<1024);
            }
            drop((schemas,channels,delivery));
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
        }
    }
}

#[test]
fn binding_error_construction_propagation_and_rejection_allocate_nothing() {
    use mcap::storage::{BudgetLimits, BudgetRef, StorageLimit};
    let empty = Value::Null;
    let check_domain = BudgetRef::new(Default::default()).unwrap();
    for case in 0..8 {
        let mut response = Response::default();
        COUNTS.with(|c| c.set(Some((0, 0))));
        let status = guard(&mut response, |_| {
            match case {
                0 => Err("Invalid input buffer".into()),
                1 => { memory::check(&check_domain, "BufferedSort", Some(128), 256)?; Ok(0) },
                2 => { string(&empty, "path")?; Ok(0) },
                3 => { number(&empty, "length")?; Ok(0) },
                4 => Err(mcap::McapError::UnknownChannel(42, 7).into()),
                5 => { BudgetRef::new(BudgetLimits {total: 0, block: 0, retained: 0})?; Ok(0) },
                6 => Err(std::io::Error::from_raw_os_error(5).into()),
                _ => Err(Error::message(format_args!("Invalid {} {}", "range", usize::MAX))),
            }
        });
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        assert_eq!(counts, (0,0), "case {case}");
        assert_eq!(status, -1);
        let parsed: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        if case == 5 {
            assert_eq!(parsed["details"]["resource"], "NativeDomain");
            assert_eq!(parsed["details"]["limit"], 0);
        }
    }
    // The failure is raised before any domain allocation, including construction
    // and formatting of the capacity diagnostic.
    let mut handle = ptr::null_mut();
    let mut id = 0;
    let mut response = Response::default();
    COUNTS.with(|c| c.set(Some((0,0))));
    let status = unsafe { budget::fm_budget_open(1, 1, 0, &mut handle, &mut id, &mut response) };
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    assert_eq!(status, -1);
    assert!(handle.is_null());
    assert_eq!(id, 0);
    let parsed: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
    assert_eq!(parsed["details"]["phase"], "budget-control");

    // Upstream io::Error construction still owns a heap payload. Measure only
    // propagation here; the binding must not allocate another wrapping error.
    let error: Error = std::io::Error::new(std::io::ErrorKind::WouldBlock,
        StorageLimit {resource:"NativeDomain",limit:1024,domain_limit:1024,requested:256,current:1024,phase:"reserve"}).into();
    assert!(budget::unavailable(&error));
    COUNTS.with(|c| c.set(Some((0,0))));
    let error = budget::after_advance(error);
    let unavailable = budget::unavailable(&error);
    let requested = budget::requested_capacity(&error);
    let status = guard(&mut response, |_| Err(error));
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    assert!(!unavailable);
    assert_eq!(requested, Some(256));
    assert_eq!(status, -1);
    let parsed: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
    assert_eq!(parsed["details"]["requested"], 256);
    assert_eq!(parsed["details"]["phase"], "reserve");

    for mask in [0, 1] {
        COUNTS.with(|c| c.set(Some((0,0))));
        let status = writer_guard(&mut response, |_| {
            registration_result::<()>(Err(mcap::McapError::InvalidSchemaId), mask, true, true)?;
            Ok(0)
        });
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        assert_eq!(counts, (0,0));
        assert_eq!(status, if mask == 0 { -1 } else { -2 });
        let parsed: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        assert_eq!(parsed["kind"], "InvalidSchemaId");
    }
    let diagnostic = "中文\"\\\n".repeat(256);
    COUNTS.with(|c| c.set(Some((0,0))));
    let status = guard(&mut response, |_| Err(diagnostic.as_str().into()));
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    assert_eq!(status, -1);
    let parsed: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
    let message = parsed["message"].as_str().unwrap();
    assert!(message.ends_with("... [truncated]"));
    assert!(diagnostic.starts_with(message.strip_suffix("... [truncated]").unwrap()), "{message:?}");
}

#[test]
fn fixed_error_responses_allocate_no_native_heap_and_preserve_details() {
    use mcap::McapError::*;
    let cases: Vec<Error> = vec![
        Error::from(BadChunkCrc { saved: 12, calculated: 34 }),
        Error::from(UnknownChannel(u32::MAX, u16::MAX)),
        Error::from(UnknownSchema("quote\"\\\n中文".into(), 42)),
        Error::from(UnsupportedCompression("lz?".into())),
        Error::from(Io(std::io::Error::other("injected failure"))),
        Error::from(memory::Limit {resource:"BufferedSort",limit:128,domain_limit:65536,current:1024,requested:256}),
        Error::from(std::io::Error::other(mcap::storage::StorageLimit {
            resource:"NativeDomain",limit:65536,domain_limit:65536,requested:8192,current:65536,phase:"wait-admission",
        })),
    ];
    for error in cases {
        let expected: Value = serde_json::from_slice(&errors::legacy_encode(error.as_ref())).unwrap();
        let mut response = Response::default();
        COUNTS.with(|c| c.set(Some((0, 0))));
        let status = guard(&mut response, |_| Err(error));
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        assert_eq!(counts, (0,0));
        assert_eq!(status,-1);
        assert!(response.json.is_null() && response.data.is_null());
        let actual: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        assert_eq!(actual, expected);
    }
    let parse = mcap::parse_record(records::op::MESSAGE, &[]).unwrap_err();
    let mut parsed = errors::FixedError::default();
    COUNTS.with(|c| c.set(Some((0,0))));
    errors::encode_into(&mut parsed, &parse);
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    for code in [5, -1, i32::MAX] {
        let error = Io(std::io::Error::from_raw_os_error(code));
        let mut response = Response::default();
        COUNTS.with(|c| c.set(Some((0, 0))));
        errors::encode_into(&mut response.error, &error);
        let counts = COUNTS.with(|c| c.replace(None).unwrap());
        assert_eq!(counts, (0,0));
        let actual: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        assert_eq!(actual["kind"], "Io");
        assert_eq!(actual["details"]["osCode"], code);
    }
    let error = ConflictingChannels("\u{1}\"\\中文😀".repeat(4096));
    let mut response = Response::default();
    COUNTS.with(|c| c.set(Some((0, 0))));
    errors::encode_into(&mut response.error, &error);
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    let actual: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
    assert_eq!(actual["kind"], "ConflictingChannels");
    assert_eq!(actual["details"]["truncated"], true);
    assert!(actual["message"].as_str().unwrap().ends_with("[truncated]"));
    assert!(response.error.length <= errors::ERROR_CAPACITY);
    COUNTS.with(|c| c.set(Some((0, 0))));
    response.error.panic();
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts, (0,0));
    assert_eq!(response.error.as_bytes(), b"Native MCAP panic; operation failed");
}

fn budget_registry_controls_have_exact_identity_and_zero_allocation_notifications() {
    let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
    domain.observe_allocations(identity::claimed);
    let audit = identity::start(mcap::storage::BudgetRef::as_ptr(&domain) as usize);
    COUNTS.with(|c| c.set(Some((0,0))));
    let handle = budget::publish(&domain).unwrap();
    let counts = COUNTS.with(|c| c.replace(None).unwrap());
    let actual = identity::snapshot();
    assert_eq!(actual.errors,0);
    assert_eq!((actual.bound,actual.bound_bytes),counts);
    assert_eq!(counts.0,1);
    assert_eq!(counts.1,domain.workload_statistics().current);
    let charge = domain.workload_detailed_statistics();
    assert_eq!(charge.resources[mcap::storage::ResourceCategory::Scratch as usize].live, counts.1);
    unsafe extern "C" fn notified(_:u64) {}
    let mut response=Response::default();
    let reserve=domain.reserve(4096).unwrap();
    COUNTS.with(|c| c.set(Some((0,0))));
    unsafe { assert_eq!(budget::fm_budget_notify(handle,notified,&mut response),0); }
    drop(reserve);
    budget::dispatch_notifications();
    let counts=COUNTS.with(|c| c.replace(None).unwrap());
    assert_eq!(counts,(0,0));
    unsafe {budget::fm_budget_free(handle);}
    assert_eq!(domain.workload_statistics().current,0);
    assert_eq!(identity::snapshot().errors,0);
    assert_eq!(identity::snapshot().live,[0;9]);
    drop(audit);
}

fn codec_callback_failures_do_not_allocate_error_payloads() {
    use mcap::{codec_allocation_probe::{Probe, Failure}, storage::{BudgetLimits, BudgetRef, ResourceCategory}};
    for category in [ResourceCategory::CodecEncoder, ResourceCategory::CodecDecoder] {
        let domain = BudgetRef::new(BudgetLimits {
            total: 16384 + BudgetRef::allocation_size(), block: 1024, retained: 0,
        }).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(domain.as_ptr() as usize);
        COUNTS.with(|c| c.set(Some((0,0))));
        let probe = Probe::new(domain.clone(), category).unwrap();
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let state = identity::snapshot();
        assert_eq!(calls, (state.bound, state.bound_bytes));
        assert_eq!(state.bound, 1);
        assert_eq!(state.errors, 0);
        assert_eq!(state.live, domain.workload_detailed_statistics().resources.map(|r|r.live));
        let baseline = domain.statistics().current;
        for zero in [false, true] {
            // Exercise integer overflow, budget denial and injected system
            // allocation failure separately; each callback must fail without
            // any actual heap allocation, including its stored diagnostic.
            for mode in 0..3 {
                if mode == 2 { domain.fail_allocation_at(0); }
                COUNTS.with(|c| c.set(Some((0,0))));
                let result = probe.exercise(match mode {0=>usize::MAX,1=>32768,_=>256}, zero);
                let calls = COUNTS.with(|c| c.replace(None).unwrap());
                assert_eq!(calls, (0,0), "category={category:?}, mode={mode}, zero={zero}");
                match (mode, result) {
                    (0, Err(Failure::Overflow)) => {},
                    (1, Err(Failure::Capacity(limit))) => {
                        assert_eq!(limit.resource, if matches!(category, ResourceCategory::CodecEncoder) {"CodecEncoder"} else {"CodecDecoder"});
                        assert!(limit.requested > 32768);
                        assert_eq!(limit.limit, domain.limits().total);
                        assert_eq!(limit.current, baseline as usize);
                        assert_eq!(limit.phase, "reservation");
                    },
                    (2, Err(Failure::Allocation(std::io::ErrorKind::OutOfMemory))) => {},
                    (_, result) => panic!("unexpected callback outcome {result:?}"),
                }
                assert_eq!(domain.statistics().current, baseline);
                assert_eq!(identity::snapshot().live, state.live);
            }
            domain.fail_allocation_at(usize::MAX);
            for bytes in [0, 1, 4096] {
                let before = identity::snapshot();
                COUNTS.with(|c| c.set(Some((0,0))));
                let capacity = probe.exercise(bytes, zero).unwrap();
                let calls = COUNTS.with(|c| c.replace(None).unwrap());
                let after = identity::snapshot();
                assert_eq!(calls, (1, capacity as u64));
                assert_eq!(after.bound, before.bound + 1);
                assert_eq!(after.bound_bytes, before.bound_bytes + capacity as u64);
                assert_eq!(after.live, state.live);
                assert_eq!(after.errors, 0);
                assert_eq!(domain.statistics().current, baseline);
            }
        }
        drop(probe);
        assert_eq!(identity::snapshot().live, [0;9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
    }
}

fn encoder_failure_transport_has_no_unattributed_allocations() {
    use mcap::storage::{BudgetRef, BudgetLimits};
    let payload = [37; 65536];
    for kind in 0..4 {
        let baseline = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
        mcap::codec_allocation_probe::encode(baseline.clone(), kind, &payload).unwrap();
        let attempts = baseline.allocation_attempt_count();
        assert!(attempts > 2);
        for failed in 0..=attempts {
            let domain = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
            domain.fail_allocation_at(failed);
            domain.observe_allocations(identity::claimed);
            let audit = identity::start(domain.as_ptr() as usize);
            let mut response = Response::default();
            COUNTS.with(|c| c.set(Some((0,0))));
            let result = mcap::codec_allocation_probe::encode(domain.clone(), kind, &payload);
            let status = guard(&mut response, |_| {result?; Ok(0)});
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            let actual = identity::snapshot();
            let stats = domain.workload_detailed_statistics();
            assert_eq!(calls, (actual.measured_bound, actual.measured_bytes), "encoder={kind}, refusal={failed}");
            assert_eq!((stats.allocation_count, stats.allocated_bytes), (actual.bound, actual.bound_bytes));
            assert_eq!(actual.live, [0;9]);
            assert_eq!(actual.errors, 0);
            assert_eq!(domain.workload_statistics().current, 0);
            if domain.allocation_attempt_count() > failed {
                assert_eq!(status, -1, "encoder={kind}, refusal={failed}");
                let error: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
                assert_eq!(error["details"]["failureKind"], "system");
                assert_eq!(error["details"]["terminal"], true);
            } else { assert_eq!(status, 0); }
            drop(audit);
        }
    }
}

fn buffered_writer_output_failures_have_exact_allocation_identities() {
    use mcap::storage::{BudgetRef, BudgetLimits};
    fn setup(domain: &BudgetRef, compression: Option<mcap::Compression>) -> (mcap::Writer<memory::Measure>, records::MessageHeader) {
        let mut writer = mcap::WriteOptions::new().compression(compression).compression_threads(0)
            .disable_seeking(true).emit_message_indexes(false).chunk_size(None)
            .memory_budget(domain.clone()).create(memory::Measure::default()).unwrap();
        let channel_id = writer.add_channel(0, "output", "raw", &BTreeMap::new()).unwrap();
        let header = records::MessageHeader {channel_id, sequence:0, log_time:1, publish_time:1};
        writer.write_to_known_channel(&header, &[0;32]).unwrap();
        (writer, header)
    }
    let mut payload = vec![0; 262144]; // Explicit caller-owned test input.
    let mut random = 42u32;
    for byte in &mut payload { random ^= random << 13; random ^= random >> 17; random ^= random << 5; *byte = random as u8; }
    for compression in [None, Some(mcap::Compression::Lz4), Some(mcap::Compression::Zstd)] {
        let baseline = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
        let (mut writer, header) = setup(&baseline, compression);
        baseline.fail_allocation_at(usize::MAX);
        writer.write_to_known_channel(&header, &payload).unwrap();
        let attempts = baseline.allocation_attempt_count();
        assert!(attempts > 0);
        writer.into_inner();
        for failed in 0..=attempts {
            let domain = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
            let (mut writer, header) = setup(&domain, compression);
            domain.fail_allocation_at(failed);
            domain.observe_allocations(identity::claimed);
            let audit = identity::start(domain.as_ptr() as usize);
            let mut response = Response::default();
            COUNTS.with(|c| c.set(Some((0,0))));
            let result = writer.write_to_known_channel(&header, &payload);
            let status = guard(&mut response, |_| {result?; Ok(0)});
            writer.into_inner();
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            let actual = identity::snapshot();
            assert_eq!(calls, (actual.measured_bound, actual.measured_bytes), "buffered={compression:?}, refusal={failed}");
            assert_eq!(actual.errors, 0);
            assert_eq!(actual.live, [0;9]);
            assert_eq!(domain.workload_statistics().current, 0);
            if failed < attempts {
                assert_eq!(status, -1);
                let error: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
                assert_eq!(error["details"]["failureKind"], "system");
                assert_eq!(error["details"]["terminal"], true);
            } else { assert_eq!(status, 0); }
            drop(audit);
        }
    }
}

fn wait_failure_construction_and_encoding_allocate_nothing() {
    use mcap::storage::{BudgetLimits, BudgetRef, CapacityWaitStatus, CapacityWaitTicket, OwnerKind};
    fn encode_failure(error: mcap::storage::StorageFailure, phase: &str) {
        assert_eq!(error.details.phase, phase);
        let mut response = Response::default();
        assert_eq!(guard(&mut response, |_| Err(error.into())), -1);
        assert!(!response.error.as_bytes().is_empty());
    }
    let domain = BudgetRef::new(BudgetLimits {total:65536,block:65536,retained:0}).unwrap();
    let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
    let occupancy = domain.reserve(65536 - domain.statistics().current as usize).unwrap();
    COUNTS.with(|c| c.set(Some((0,0))));
    encode_failure(ticket.arm(usize::MAX).unwrap_err(), "wait-registration");
    assert_eq!(ticket.status(), CapacityWaitStatus::Idle);
    encode_failure(ticket.arm_for_external_release(1).unwrap_err(), "wait-admission");
    encode_failure(domain.retry_status(1).unwrap_err(), "wait-admission");
    occupancy.owner_reference(OwnerKind::Lease, true);
    assert_eq!(ticket.arm_for_external_release(1).unwrap(), CapacityWaitStatus::Waiting);
    occupancy.owner_reference(OwnerKind::Lease, false);
    assert_eq!(ticket.status(), CapacityWaitStatus::Unavailable);
    encode_failure(ticket.checked_status().unwrap_err(), "wait-admission");
    ticket.cancel();
    assert_eq!(COUNTS.with(|c| c.replace(None).unwrap()), (0,0));
    drop(occupancy);
    drop(ticket);
    assert_eq!(domain.workload_statistics().current, 0);
    for system in [false, true] {
        let limit = if system {65536} else {BudgetRef::allocation_size()};
        let domain = BudgetRef::new(BudgetLimits {total:limit,block:1,retained:0}).unwrap();
        if system {domain.fail_allocation_at(0);}
        COUNTS.with(|c| c.set(Some((0,0))));
        let error = match CapacityWaitTicket::new(&domain) {Err(error) => error, Ok(_) => panic!("expected refusal")};
        let mut response = Response::default();
        assert_eq!(guard(&mut response, |_| Err(error.into())), -1);
        assert_eq!(COUNTS.with(|c| c.replace(None).unwrap()), (0,0));
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

fn response_buffers_and_streamed_descriptions_have_exact_identities() {
    use mcap::{shared_declarations::{SharedSchema, SharedChannel},storage::{BudgetRef,OwnerKind}};
    for failed in 0..=2 {
        let domain=BudgetRef::new(Default::default()).unwrap();
        let schema=SharedSchema::new(7,"schema\"中文","raw",&[1,2,3,4],&domain,OwnerKind::Parser).unwrap();
        let channel=SharedChannel::new(9,"topic\n","raw",Some(schema),[("a","quote\""),("b","中文")].into_iter(),&domain,OwnerKind::Parser).unwrap();
        let before=domain.statistics().current;
        domain.fail_allocation_at(failed);
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(domain.as_ptr() as usize);
        let mut result=Response::default();
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=guard(&mut result,|out| {response::channel(out,&domain,&channel,true)?;Ok(0)});
        let calls=COUNTS.with(|c|c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));
        assert_eq!(actual.errors,0);
        if failed<2 {
            assert_eq!(status,-1);
            assert!(result.json.is_null() && result.data.is_null());
            assert_eq!(actual.live,[0;9]);
            assert_eq!(domain.statistics().current,before);
        } else {
            assert_eq!(status,0);
            assert_eq!(actual.bound,2);
            assert_eq!(domain.statistics().current,before+actual.bound_bytes);
            drop(channel);
            let value:Value=serde_json::from_slice(unsafe {bytes(result.json,result.json_len).unwrap()}).unwrap();
            assert_eq!(value,json!({"id":9,"topic":"topic\n","messageEncoding":"raw","metadata":{"a":"quote\"","b":"中文"},"schema":{"id":7,"name":"schema\"中文","encoding":"raw"}}));
            assert_eq!(unsafe {bytes(result.data,result.data_len).unwrap()},&[1,2,3,4]);
            let weak=domain.downgrade();drop(domain);
            assert!(weak.upgrade().is_some());
            unsafe {fm_buffer_free(result.json,result.json_len);fm_buffer_free(result.data,result.data_len);}
            assert!(weak.upgrade().is_none());
            assert_eq!(identity::snapshot().live,[0;9]);
        }
        drop(audit);
    }
}

fn decoder_failure_transport_has_no_unattributed_allocations() {
    use mcap::storage::{BudgetRef, BudgetLimits, StorageFailureKind};
    for compression in [mcap::Compression::Lz4, mcap::Compression::Zstd] {
        let mut writer = mcap::WriteOptions::new().compression(Some(compression)).chunk_size(None)
            .create(std::io::Cursor::new(Vec::new())).unwrap();
        let channel = writer.add_channel(0, "codec", "raw", &BTreeMap::new()).unwrap();
        writer.write_to_known_channel(&records::MessageHeader {channel_id:channel,sequence:0,log_time:1,publish_time:1}, &[37;65536]).unwrap();
        writer.finish().unwrap();
        let wire = writer.into_inner().into_inner();
        let chunk = mcap::read::LinearReader::new(&wire).unwrap().find_map(|record| match record.unwrap() {
            records::Record::Chunk {header, data} => Some((header.compression, data.into_owned())), _ => None,
        }).unwrap();
        let mut output = [0; 131072];
        let baseline = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
        let length = mcap::sans_io::audit_decode_twice(&chunk.0, baseline.clone(), &chunk.1, &mut output).unwrap();
        let attempts = baseline.workload_detailed_statistics().allocation_count;
        assert!(attempts >= 2);
        assert!(length >= 65536);
        assert_eq!(&output[length-65536..length], &[37;65536]);
        assert_eq!(baseline.workload_statistics().current, 0);
        for failed in 0..=attempts {
            let domain = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
            domain.fail_allocation_at(failed as usize);
            domain.observe_allocations(identity::claimed);
            let audit = identity::start(domain.as_ptr() as usize);
            let mut response = Response::default();
            COUNTS.with(|c| c.set(Some((0,0))));
            let result = mcap::sans_io::audit_decode_twice(&chunk.0, domain.clone(), &chunk.1, &mut output);
            let status = guard(&mut response, |_| { result?; Ok(0) });
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            let actual = identity::snapshot();
            assert_eq!(calls, (actual.bound, actual.bound_bytes), "decoder={compression:?}, refusal={failed}");
            assert_eq!(actual.errors, 0);
            assert_eq!(actual.live, [0;9]);
            assert_eq!(domain.workload_statistics().current, 0);
            if failed < attempts {
                assert_eq!(status, -1);
                let error: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
                assert_eq!(error["kind"], "Binding");
                assert_eq!(error["details"]["resource"], "CodecDecoder");
                assert_eq!(error["details"]["failureKind"], StorageFailureKind::SystemAllocation.as_str());
                assert_eq!(error["details"]["terminal"], true);
                assert!(error["details"]["requested"].as_u64().unwrap() > 0);
            } else { assert_eq!(status, 0); }
            drop(audit);
        }
        // Invalid frame diagnostics use codec-owned static names, not String
        // or custom io::Error allocations. Preserve the previous managed kinds.
        let mut broken = chunk.1.clone();
        broken[0] ^= 0xff;
        let domain = BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(domain.as_ptr() as usize);
        let mut response = Response::default();
        COUNTS.with(|c| c.set(Some((0,0))));
        let result = mcap::sans_io::audit_decode_twice(&chunk.0, domain.clone(), &broken, &mut output);
        let status = guard(&mut response, |_| { result?; Ok(0) });
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(status, -1);
        assert_eq!(calls, (actual.bound, actual.bound_bytes), "malformed {compression:?}");
        assert_eq!(actual.live, [0;9]);
        assert_eq!(actual.errors, 0);
        assert_eq!(domain.workload_statistics().current, 0);
        let error: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        match compression {
            mcap::Compression::Lz4 => {
                assert_eq!(error["kind"], "Io");
                assert!(error["details"]["source"].as_str().unwrap().starts_with("LZ4 error: "));
            }
            mcap::Compression::Zstd => {
                assert_eq!(error["kind"], "DecompressionError");
                assert!(!error["details"]["description"].as_str().unwrap().is_empty());
            }
        }
        drop(audit);
        // Permanent refusal before a decoder control/context can be allocated.
        let domain = BudgetRef::new(BudgetLimits {total:BudgetRef::allocation_size(),block:1,retained:0}).unwrap();
        let mut response = Response::default();
        COUNTS.with(|c| c.set(Some((0,0))));
        let result = mcap::sans_io::audit_decode_twice(&chunk.0, domain.clone(), &chunk.1, &mut output);
        let status = guard(&mut response, |_| { result?; Ok(0) });
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        assert_eq!(calls, (0,0));
        assert_eq!(status, -1);
        let error: Value = serde_json::from_slice(response.error.as_bytes()).unwrap();
        assert_eq!(error["details"]["failureKind"], "permanent");
        assert_eq!(error["details"]["terminal"], true);
        assert_eq!(domain.workload_statistics().current, 0);
    }
}

fn streamed_summary_has_one_exact_response_allocation() {
    use mcap::{storage::{BudgetRef,OwnerKind},segmented::SharedSegmentedVec,
        shared_metadata_index::SharedMetadataIndex};
    for failed in 0..=1 {
        let domain=BudgetRef::new(Default::default()).unwrap();
        let mut observed=summary_response::Observed::new(&domain);
        for i in 0..40000 {observed.schemas.push(i as u16).unwrap();}
        observed.channels.push(9).unwrap();observed.channels.push(9).unwrap();
        let chunks=SharedSegmentedVec::new(domain.clone());
        let attachments=SharedSegmentedVec::new(domain.clone());
        let mut metadata=SharedSegmentedVec::new(domain.clone());
        metadata.push(SharedMetadataIndex::new(17,23,"name\"\u{4e2d}\n",&domain,OwnerKind::Parser).unwrap()).unwrap();
        let view=summary_response::View {statistics:None,chunks:&chunks,attachments:&attachments,metadata:&metadata};
        let before=domain.statistics().current;
        domain.fail_allocation_at(failed);
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(domain.as_ptr() as usize);
        let mut result=Response::default();
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=guard(&mut result,|out| {view.respond(out,&domain,&observed)?;Ok(0)});
        let calls=COUNTS.with(|c|c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));
        assert_eq!(actual.errors,0);
        if failed==0 {
            assert_eq!(status,-1);
            assert!(result.json.is_null() && result.data.is_null());
        } else {
            assert_eq!(status,0);assert_eq!(actual.bound,1);
            let value:Value=serde_json::from_slice(unsafe {bytes(result.json,result.json_len).unwrap()}).unwrap();
            assert_eq!(value["statistics"],Value::Null);
            assert_eq!(value["schemaIds"].as_array().unwrap().len(),40000);
            assert_eq!(value["schemaIds"][39999],39999);
            assert_eq!(value["channelIds"],json!([9,9]));
            assert_eq!(value["metadataIndexes"],json!([{"offset":17,"length":23,"name":"name\"\u{4e2d}\n"}]));
            unsafe {fm_buffer_free(result.json,result.json_len);}
        }
        assert_eq!(identity::snapshot().live,[0;9]);
        assert_eq!(domain.statistics().current,before);
        drop(audit);
    }
}

fn random_record_input_has_exact_allocation_identities() {
    use mcap::storage::BudgetRef;
    unsafe extern "C" fn read(context:*mut std::ffi::c_void,dest:*mut u8,len:usize,count:*mut usize)->i32 {
        let source=&mut *context.cast::<std::io::Cursor<Vec<u8>>>();
        match source.read(std::slice::from_raw_parts_mut(dest,len.min(7))) {
            Ok(n)=>{*count=n;0},Err(_)=>-1,
        }
    }
    unsafe extern "C" fn write(_: *mut std::ffi::c_void,_:*const u8,_:usize)->i32 {-1}
    unsafe extern "C" fn seek(_: *mut std::ffi::c_void,_:i64,_:i32,_:*mut u64)->i32 {-1}
    unsafe extern "C" fn flush(_: *mut std::ffi::c_void)->i32 {0}
    let payload=vec![42;65536];
    let path=std::env::temp_dir().join(format!("fizzy-record-input-probe-{}.bin",std::process::id()));
    std::fs::write(&path,&payload).unwrap();
    for mapped in [false,true] {
        for failed in 0..=if mapped {1} else {2} {
            let domain=BudgetRef::new(Default::default()).unwrap();
            let options=memory::Options::with_domain(domain.clone());
            let mut cursor=std::io::Cursor::new(payload.clone());
            let mut input=if mapped {open_input(path.to_str().unwrap(),&domain).unwrap()} else {
                Input::Stream(Callbacks {context:(&mut cursor as *mut std::io::Cursor<Vec<u8>>).cast(),read,write,seek,flush,seekable:0})
            };
            let before=domain.statistics().current;
            domain.fail_allocation_at(failed);
            domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|out| {
                let body=record_input::Body::read(&mut input,0,payload.len(),&options)?;
                assert_eq!(body.as_ref(),&payload);
                response::write(out,&domain,|_|Ok(()),body.as_ref(),0)?;Ok(0)
            });
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes));
            assert_eq!(actual.errors,0);
            if failed < if mapped {1} else {2} {
                assert_eq!(status,-1);assert!(output.data.is_null());
            } else {
                assert_eq!(status,0);assert_eq!(actual.bound,if mapped {1} else {2});
                assert_eq!(unsafe {bytes(output.data,output.data_len).unwrap()},&payload);
                unsafe {fm_buffer_free(output.data,output.data_len);}
            }
            assert_eq!(identity::snapshot().live,[0;9]);
            assert_eq!(domain.statistics().current,before);
            drop(audit);
        }
    }
    std::fs::remove_file(path).unwrap();
}

fn string_map_validation_has_exact_allocation_identities() {
    use mcap::storage::BudgetRef;
    for (opcode,wire) in record_input::fixtures().into_iter().filter(|(op,_)|matches!(*op,records::op::CHANNEL|records::op::METADATA)) {
        let domain=BudgetRef::new(Default::default()).unwrap();
        record_input::validate(opcode,&wire,&domain).unwrap();
        let allocations=domain.workload_detailed_statistics().allocation_count as usize;
        assert!(allocations>0);
        for failed in 0..=allocations {
            let domain=BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(failed);
            domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {record_input::validate(opcode,&wire,&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes),"opcode={opcode} failure={failed}");
            assert_eq!(actual.errors,0);
            assert_eq!(status,if failed<allocations {-1} else {0});
            assert_eq!(actual.live,[0;9]);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
            drop(audit);
        }
    }
}

fn borrowed_record_validation_has_no_heap_allocations() {
    let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
    for (opcode,wire) in record_input::borrowed_fixtures().into_iter().chain(record_input::fixtures().into_iter().filter(|(op,_)|!matches!(*op,records::op::CHANNEL|records::op::METADATA))) {
        for end in 0..=wire.len() {
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {record_input::validate(opcode,&wire[..end],&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            assert_eq!(calls,(0,0),"opcode={opcode} end={end} status={status}");
            assert_eq!(domain.workload_statistics().current,0);
        }
        for position in 0..wire.len() {
            let mut malformed=wire.clone();malformed[position]^=0xff;
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {record_input::validate(opcode,&malformed,&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            assert_eq!(calls,(0,0),"opcode={opcode} position={position} status={status}");
        }
    }
}

fn metadata_duplicate_error_has_no_unaccounted_allocations() {
    let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
    let mut wire=Vec::from(0u32.to_le_bytes());wire.extend_from_slice(&20u32.to_le_bytes());
    for value in [b'a',b'b'] {
        wire.extend_from_slice(&1u32.to_le_bytes());wire.push(b'k');
        wire.extend_from_slice(&1u32.to_le_bytes());wire.push(value);
    }
    assert!(mcap::parse_record(records::op::METADATA,&wire).is_err());
    domain.observe_allocations(identity::claimed);
    let audit=identity::start(domain.as_ptr() as usize);
    let mut output=Response::default();
    COUNTS.with(|c|c.set(Some((0,0))));
    let status=guard(&mut output,|_| {record_input::validate(records::op::METADATA,&wire,&domain)?;Ok(0)});
    let calls=COUNTS.with(|c|c.replace(None).unwrap());
    let actual=identity::snapshot();
    assert_eq!(calls,(actual.bound,actual.bound_bytes));
    assert_eq!(actual.errors,0);assert_eq!(status,-1);
    assert_eq!(actual.live,[0;9]);assert_eq!(domain.workload_statistics().current,0);
    let error:Value=serde_json::from_slice(output.error.as_bytes()).unwrap();
    assert_eq!(error["kind"],"Parse");assert_eq!(error["details"]["position"],8);
    assert_eq!(error["details"]["source"],"Duplicate keys in map");
    drop(audit);
}

fn real_budget_rejection_in_string_maps_has_no_error_allocations() {
    use mcap::storage::{BudgetRef,BudgetLimits};
    for (opcode,wire) in record_input::fixtures().into_iter().filter(|(op,_)|matches!(*op,records::op::CHANNEL|records::op::METADATA)) {
        let baseline=BudgetRef::new(Default::default()).unwrap();
        record_input::validate(opcode,&wire,&baseline).unwrap();
        let required=baseline.workload_statistics().peak as usize;
        for capacity in [0,1,128,1024,65535,65536,required-1,required] {
            let total=BudgetRef::allocation_size()+capacity;
            let domain=BudgetRef::new(BudgetLimits {total,block:1,retained:0}).unwrap();
            domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {record_input::validate(opcode,&wire,&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes),"opcode={opcode} capacity={capacity}");
            assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
            assert!(domain.statistics().peak<=total as u64);
            assert_eq!(status,if capacity<required {-1} else {0});
            if status<0 {
                let error:Value=serde_json::from_slice(output.error.as_bytes()).unwrap();
                assert_eq!(error["kind"],"Binding");
                assert_eq!(error["details"]["phase"],"reservation");
                assert_eq!(error["details"]["domainLimit"],total);
                assert!(matches!(error["details"]["failureKind"].as_str(),Some("permanent"|"temporary")));
                assert!(error["details"]["requested"].as_u64().unwrap()>0);
            }
            drop(audit);
        }
    }
}

fn real_budget_rejection_in_declarations_has_no_error_allocations() {
    use mcap::{storage::{BudgetRef,BudgetLimits,OwnerKind},shared_declarations::{SharedSchema,SharedChannel}};
    fn read(op:u8,wire:&[u8],domain:&BudgetRef)->Outcome<()> {
        if op==records::op::SCHEMA {
            let mut table=mcap::u16_table::SharedU16Table::new(domain.clone(),mcap::storage::ResourceCategory::Declaration);
            let schema=SharedSchema::read(wire,domain,OwnerKind::Parser)?;
            table.insert_fixed(schema.id,schema)?;
        } else {
            let mut table=mcap::u16_table::SharedU16Table::new(domain.clone(),mcap::storage::ResourceCategory::Declaration);
            let channel=SharedChannel::read_record(wire,domain,OwnerKind::Parser)?;
            table.insert_fixed(channel.id,channel)?;
        }
        Ok(())
    }
    for (opcode,wire) in record_input::fixtures().into_iter().chain(record_input::borrowed_fixtures()).filter(|(op,_)|matches!(*op,records::op::CHANNEL|records::op::SCHEMA)) {
        let baseline=BudgetRef::new(Default::default()).unwrap();
        read(opcode,&wire,&baseline).unwrap();
        let required=baseline.workload_statistics().peak as usize;
        for capacity in [0,1,4,8,12,32,128,1024,65535,65536,required-1,required] {
            let total=BudgetRef::allocation_size()+capacity;
            let domain=BudgetRef::new(BudgetLimits {total,block:1,retained:0}).unwrap();
            domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {read(opcode,&wire,&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes),"opcode={opcode} capacity={capacity}");
            assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
            assert!(domain.statistics().peak<=total as u64);
            assert_eq!(status,if capacity<required {-1} else {0});
            if status<0 {
                let error:Value=serde_json::from_slice(output.error.as_bytes()).unwrap();
                assert_eq!(error["kind"],"Binding");
                assert_eq!(error["details"]["phase"],"reservation");
                assert_eq!(error["details"]["domainLimit"],total);
            }
            drop(audit);
        }
    }
}

fn real_budget_rejection_in_shared_indexes_has_no_error_allocations() {
    use mcap::{storage::{BudgetRef,BudgetLimits,OwnerKind},segmented::SharedSegmentedVec};
    fn read(op:u8,wire:&[u8],domain:&BudgetRef)->Outcome<()> {
        match op {
            records::op::CHUNK_INDEX=>{
                let mut values=SharedSegmentedVec::new(domain.clone());
                values.push_fixed(mcap::shared_chunk_index::SharedChunkIndex::read(wire,domain,OwnerKind::Parser)?)?;
            }
            records::op::ATTACHMENT_INDEX=>{
                let mut values=SharedSegmentedVec::new(domain.clone());
                values.push_fixed(mcap::shared_attachment_index::SharedAttachmentIndex::read(wire,domain,OwnerKind::Parser)?)?;
            }
            records::op::METADATA_INDEX=>{
                let mut values=SharedSegmentedVec::new(domain.clone());
                values.push_fixed(mcap::shared_metadata_index::SharedMetadataIndex::read(wire,domain,OwnerKind::Parser)?)?;
            }
            records::op::STATISTICS=>{mcap::shared_statistics::SharedStatistics::read(&mut std::io::Cursor::new(wire),domain.clone(),OwnerKind::Parser)?;}
            _=>unreachable!(),
        }
        Ok(())
    }
    for (opcode,wire) in record_input::fixtures().into_iter().filter(|(op,_)|matches!(*op,records::op::CHUNK_INDEX|records::op::ATTACHMENT_INDEX|records::op::METADATA_INDEX|records::op::STATISTICS)) {
        let baseline=BudgetRef::new(Default::default()).unwrap();
        read(opcode,&wire,&baseline).unwrap();
        let required=baseline.workload_statistics().peak as usize;
        for capacity in [0,1,128,1024,16384,65535,65536,required-1,required] {
            let total=BudgetRef::allocation_size()+capacity;
            let domain=BudgetRef::new(BudgetLimits {total,block:1,retained:0}).unwrap();
            domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut output,|_| {read(opcode,&wire,&domain)?;Ok(0)});
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes),"opcode={opcode} capacity={capacity}");
            assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
            assert!(domain.statistics().peak<=total as u64);
            assert_eq!(status,if capacity<required {-1} else {0});
            if status<0 {
                let error:Value=serde_json::from_slice(output.error.as_bytes()).unwrap();
                assert_eq!(error["kind"],"Binding");
                assert_eq!(error["details"]["phase"],"reservation");
                assert_eq!(error["details"]["domainLimit"],total);
            }
            drop(audit);
        }
    }
}

fn fixed_shared_controls_have_no_error_allocations() {
    use mcap::{charged::weak::BudgetedArc, storage::{BudgetRef, BudgetLimits, ResourceCategory, OwnerKind}};
    let required = BudgetedArc::<u64>::allocation_size();
    for capacity in [0, required - 1, required] {
        for injected in [false, true] {
            let total = BudgetRef::allocation_size() + capacity;
            let domain = BudgetRef::new(BudgetLimits {total, block: 1, retained: 0}).unwrap();
            domain.observe_allocations(identity::claimed);
            if injected { domain.fail_allocation_at(0); }
            let audit = identity::start(domain.as_ptr() as usize);
            let mut output = Response::default();
            COUNTS.with(|c| c.set(Some((0, 0))));
            let status = guard(&mut output, |_| {
                let value = BudgetedArc::new_with_owner_fixed(42u64, &domain,
                    ResourceCategory::Declaration, OwnerKind::Parser)?;
                let weak = value.downgrade();
                drop(value);
                assert!(weak.upgrade().is_none());
                drop(weak);
                Ok(0)
            });
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            let actual = identity::snapshot();
            assert_eq!(calls, (actual.bound, actual.bound_bytes));
            assert_eq!(actual.errors, 0);
            assert_eq!(actual.live, [0; 9]);
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
            assert!(domain.statistics().peak <= total as u64);
            assert_eq!(status, if capacity < required || injected {-1} else {0});
            drop(audit);
        }
    }
}

fn fixed_registry_refusals_have_no_error_allocations() {
    use mcap::{charged::weak::BudgetedArc, storage::{BudgetRef, BudgetLimits, ResourceCategory}};
    struct Candidate;
    impl mcap::storage::Reclaimable for Candidate {
        fn last_access(&self) -> u64 {0}
        fn reclaim(&self) -> bool {false}
    }
    fn register(domain: &BudgetRef) -> Outcome<()> {
        let value = BudgetedArc::new_fixed(Candidate, domain, ResourceCategory::Scratch)?;
        domain.register_reclaimer_fixed(value.reclaimer())?;
        drop(value);
        domain.prune_reclaimers();
        Ok(())
    }
    let baseline = BudgetRef::new(Default::default()).unwrap();
    register(&baseline).unwrap();
    let required = baseline.workload_statistics().peak as usize;
    for capacity in [0, BudgetedArc::<Candidate>::allocation_size(), required - 1, required] {
        for refusal in [None, Some(0), Some(1)] {
            let total = BudgetRef::allocation_size() + capacity;
            let domain = BudgetRef::new(BudgetLimits {total, block: 1, retained: 0}).unwrap();
            domain.observe_allocations(identity::claimed);
            if let Some(index) = refusal {domain.fail_allocation_at(index);}
            let audit = identity::start(domain.as_ptr() as usize);
            let mut output = Response::default();
            COUNTS.with(|c| c.set(Some((0, 0))));
            let status = guard(&mut output, |_| {register(&domain)?; Ok(0)});
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            let actual = identity::snapshot();
            assert_eq!(calls, (actual.bound, actual.bound_bytes));
            assert_eq!(actual.errors, 0);
            assert_eq!(actual.live, [0; 9]);
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
            assert!(domain.statistics().peak <= total as u64);
            assert_eq!(status, if capacity < required || refusal.is_some() {-1} else {0});
            drop(audit);
        }
    }
}

fn borrowed_chunk_headers_have_no_heap_allocations() {
    for (opcode, wire) in record_input::borrowed_fixtures() {
        if opcode != records::op::CHUNK {continue;}
        for end in 0..=wire.len() {
            let expected = mcap::parse_record(opcode, &wire[..end]).is_ok();
            let mut output = Response::default();
            COUNTS.with(|c| c.set(Some((0, 0))));
            let status = guard(&mut output, |_| {
                let (header, data) = mcap::read::parse_borrowed_chunk(&wire[..end])?;
                assert_eq!(data.len() as u64, header.compressed_size);
                assert!(data.as_ptr() >= wire.as_ptr());
                Ok(0)
            });
            let calls = COUNTS.with(|c| c.replace(None).unwrap());
            assert_eq!(calls, (0, 0));
            assert_eq!(status == 0, expected);
        }
    }
}

fn indexed_bodies_have_charged_allocations_without_payload_copies() {
    use binrw::BinWrite;
    let mut attachment = std::io::Cursor::new(Vec::new());
    records::AttachmentHeader {log_time:1,create_time:2,name:"large".into(),media_type:"raw".into()}.write_le(&mut attachment).unwrap();
    let mut attachment=attachment.into_inner();
    attachment.extend_from_slice(&(8u64 * 1024 * 1024).to_le_bytes());
    attachment.resize(attachment.len()+8*1024*1024,42);
    attachment.extend_from_slice(&crc32fast::hash(&attachment).to_le_bytes());
    for (opcode, body) in record_input::fixtures().into_iter().chain(record_input::borrowed_fixtures())
        .filter(|(op,_)| matches!(*op,records::op::METADATA|records::op::ATTACHMENT))
        .chain(std::iter::once((records::op::ATTACHMENT,attachment))) {
        let mut wire=vec![opcode];wire.extend_from_slice(&(body.len() as u64).to_le_bytes());wire.extend_from_slice(&body);
        let options=memory::Options::default();
        let source=memory::Source::new(memory::Backing::copy(&wire, options.clone()).unwrap(), &options.domain).unwrap();
        let baseline=options.domain.workload_statistics().current;
        let before=options.domain.detailed_statistics().flow;
        options.domain.observe_allocations(identity::claimed);
        let audit=identity::start(options.domain.as_ptr() as usize);
        COUNTS.with(|c| c.set(Some((0,0))));
        let result=record_input::indexed_body(&source,0,wire.len() as u64,opcode,&options.domain).unwrap();
        assert_eq!(result.as_ref().as_ptr(), unsafe {source.as_ptr().add(9)});
        drop(result);
        let calls=COUNTS.with(|c| c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));
        assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
        assert_eq!(options.domain.workload_statistics().current,baseline);
        let after=options.domain.detailed_statistics().flow;
        assert_eq!(before.input_copy,after.input_copy);
        assert_eq!(before.other_copy,after.other_copy);
        drop(audit);
        for refused in 0..actual.bound as usize {
            options.domain.fail_allocation_at(refused);
            let audit=identity::start(options.domain.as_ptr() as usize);
            let mut output=Response::default();
            COUNTS.with(|c| c.set(Some((0,0))));
            let status=guard(&mut output, |_| {
                record_input::indexed_body(&source,0,wire.len() as u64,opcode,&options.domain)?;
                Ok(0)
            });
            let calls=COUNTS.with(|c| c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes));
            assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
            assert_eq!(status,-1);
            assert_eq!(options.domain.workload_statistics().current,baseline);
            drop(audit);
        }
    }
}

fn configured_json_uses_selected_budget_before_allocation() {
    let mut response=Response::default();
    let mut handle=ptr::null_mut();let mut id=0;
    unsafe {assert_eq!(budget::fm_budget_open(1024*1024,1024*1024,0,&mut handle,&mut id,&mut response),0);}
    let domain=budget::resolve(id).unwrap();
    let input=format!(r#"{{"Budget":{{"id":{id}}},"MaxRandomAccessCacheBytes":1024,"unused":"escaped \u4e2d"}}"#);
    let baseline=domain.statistics().current;
    domain.observe_allocations(identity::claimed);
    for refused in [false,true] {
        let blocker=if refused {Some(domain.reserve(domain.limits().total-domain.statistics().current as usize).unwrap())} else {None};
        let audit=identity::start(domain.as_ptr() as usize);
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=guard(&mut response,|_| {
            let options=memory::Options::parse_config(input.as_bytes())?;
            assert!(mcap::storage::BudgetRef::ptr_eq(&domain,&options.domain));
            assert_eq!(options.random,1024);Ok(0)
        });
        let calls=COUNTS.with(|c|c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
        assert_eq!(status,if refused {-1} else {0});
        drop(audit);drop(blocker);
        assert_eq!(domain.statistics().current,baseline);
    }
    for input in [br#"{"Bu\u0064get":{"id":7}}"#.as_slice(),br#"{"a":"\ud800"}"#,br#"{"Budget":{"id":1e+}}"#] {
        COUNTS.with(|c|c.set(Some((0,0))));
        guard(&mut response,|_| {budget_json::select_id(input,&["Budget","id"])?;Ok(0)});
        assert_eq!(COUNTS.with(|c|c.replace(None).unwrap()),(0,0));
    }
    unsafe {budget::fm_budget_free(handle);}
    assert_eq!(domain.workload_statistics().current,0);
}

fn writer_option_text_has_exact_shared_allocation_identity() {
    use mcap::storage::{BudgetRef,BudgetLimits};
    fn make(domain:&BudgetRef)->Outcome<mcap::WriteOptions> {
        Ok(mcap::WriteOptions::new().memory_budget(domain.clone()).compression(None)
            .try_profile("profile with unicode \u{4e2d}")?.try_library("independent library")?)
    }
    let baseline=BudgetRef::new(Default::default()).unwrap();
    drop(make(&baseline).unwrap());
    let required=baseline.workload_statistics().peak as usize;
    let allocations=baseline.workload_detailed_statistics().allocation_count as usize;
    for capacity in [0,1,required-1,required] {
        for failure in 0..=allocations {
            let total=BudgetRef::allocation_size()+capacity;
            let domain=BudgetRef::new(BudgetLimits {total,block:1,retained:0}).unwrap();
            domain.fail_allocation_at(failure);domain.observe_allocations(identity::claimed);
            let audit=identity::start(domain.as_ptr() as usize);
            let mut response=Response::default();
            COUNTS.with(|c|c.set(Some((0,0))));
            let status=guard(&mut response,|_| {
                let options=make(&domain)?;let alias=options.clone();drop(options);drop(alias);Ok(0)
            });
            let calls=COUNTS.with(|c|c.replace(None).unwrap());
            let actual=identity::snapshot();
            assert_eq!(calls,(actual.bound,actual.bound_bytes));assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
            assert_eq!(status,if capacity<required || failure<allocations {-1} else {0});
            assert_eq!(domain.workload_statistics().current,0);assert_eq!(domain.ownership_statistics(),Default::default());
            assert!(domain.statistics().peak<=total as u64);
            drop(audit);
        }
    }
}

fn writer_constructor_control_and_text_are_fully_charged() {
    unsafe extern "C" fn read(_: *mut std::ffi::c_void,_:*mut u8,_:usize,n:*mut usize)->i32 {*n=0;0}
    unsafe extern "C" fn write(p:*mut std::ffi::c_void,_:*const u8,n:usize)->i32 {*(p as *mut u64)+=n as u64;0}
    unsafe extern "C" fn seek(p:*mut std::ffi::c_void,n:i64,origin:i32,out:*mut u64)->i32 {
        let position=&mut *(p as *mut u64);
        *position=if origin==0 {n as u64} else {(*position as i64+n) as u64};*out=*position;0
    }
    unsafe extern "C" fn flush(_: *mut std::ffi::c_void)->i32 {0}
    let mut response=Response::default();let mut handle=ptr::null_mut();let mut id=0;
    unsafe {assert_eq!(budget::fm_budget_open(1024*1024,1024*1024,0,&mut handle,&mut id,&mut response),0);}
    let domain=budget::resolve(id).unwrap();
    let input=format!(r#"{{"options":{{"compression":"none","useChunks":false,"profile":"profile","library":"test library","memory":{{"budget":{{"id":{id}}}}}}}}}"#);
    let baseline=domain.statistics().current;
    domain.observe_allocations(identity::claimed);
    let mut allocation_count=0;
    for iteration in 0..64 {
        if iteration>0 && iteration>allocation_count {break;}
        domain.fail_allocation_at(if iteration==0 {usize::MAX} else {iteration-1});
        let mut position=0u64;
        let callbacks=Callbacks {context:(&mut position as *mut u64).cast(),read,write,seek,flush,seekable:1};
        let mut writer=ptr::null_mut();
        let audit=identity::start(domain.as_ptr() as usize);
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=unsafe {fm_writer_open(input.as_ptr(),input.len(),&callbacks,&mut writer,&mut response)};
        unsafe {fm_writer_free(writer);}
        let calls=COUNTS.with(|c|c.replace(None).unwrap());let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes),"failure iteration={iteration}");
        assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.statistics().current,baseline);
        if iteration==0 {assert_eq!(status,0);allocation_count=actual.bound as usize;assert!(allocation_count>4 && allocation_count<64);}
        else {assert_eq!(status,-1);assert!(writer.is_null());}
        drop(audit);
    }
    unsafe {budget::fm_budget_free(handle);}
    assert_eq!(domain.workload_statistics().current,0);
}

fn indexed_channel_filter_pages_have_exact_allocation_identity() {
    let mut writer=mcap::WriteOptions::new().compression(None).create(std::io::Cursor::new(Vec::new())).unwrap();
    for i in 0..600 {writer.add_channel(0,&format!("topic/{i}"),"raw",&BTreeMap::new()).unwrap();}
    writer.finish().unwrap();
    let bytes=writer.into_inner().into_inner();
    let summary=mcap::Summary::read(&bytes).unwrap().unwrap();
    let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
    domain.observe_allocations(identity::claimed);
    let options=sans_io::IndexedReaderOptions::new().include_topics(["topic/0","topic/256","topic/599"]);
    let audit=identity::start(domain.as_ptr() as usize);
    COUNTS.with(|c|c.set(Some((0,0))));
    let reader=sans_io::IndexedReader::new_with_options_and_budget(&summary,options,domain.clone()).unwrap();
    let live=identity::snapshot();
    assert_eq!(live.live,domain.workload_detailed_statistics().resources.map(|r|r.live));
    assert_eq!(domain.workload_detailed_statistics().resources.iter().map(|r|r.reserved).sum::<u64>(),0);
    drop(reader);
    let calls=COUNTS.with(|c|c.replace(None).unwrap());let actual=identity::snapshot();
    assert_eq!(calls,(actual.bound,actual.bound_bytes));assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
    assert_eq!(domain.workload_statistics().current,0);drop(audit);
}

fn indexed_constructor_capacity_errors_do_not_allocate() {
    use mcap::storage::{BudgetRef,BudgetLimits};
    let mut writer=mcap::WriteOptions::new().compression(None).create(std::io::Cursor::new(Vec::new())).unwrap();
    let channel=writer.add_channel(0,"topic","raw",&BTreeMap::new()).unwrap();
    writer.write_to_known_channel(&records::MessageHeader {channel_id:channel,sequence:0,log_time:0,publish_time:0},b"data").unwrap();
    writer.finish().unwrap();let bytes=writer.into_inner().into_inner();
    let summary=mcap::Summary::read(&bytes).unwrap().unwrap();
    let baseline=BudgetRef::new(Default::default()).unwrap();
    drop(sans_io::IndexedReader::new_with_options_and_budget(&summary,Default::default(),baseline.clone()).unwrap());
    let required=baseline.workload_statistics().peak as usize;
    for capacity in [0,1,required-1,required] {
        let total=BudgetRef::allocation_size()+capacity;
        let domain=BudgetRef::new(BudgetLimits {total,block:1,retained:0}).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(domain.as_ptr() as usize);
        let mut response=Response::default();
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=guard(&mut response,|_| {
            sans_io::IndexedReader::new_with_options_and_budget(&summary,Default::default(),domain.clone())?;Ok(0)
        });
        let calls=COUNTS.with(|c|c.replace(None).unwrap());let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));assert_eq!(actual.errors,0);assert_eq!(actual.live,[0;9]);
        assert_eq!(status,if capacity<required {-1} else {0});
        assert_eq!(domain.workload_statistics().current,0);assert!(domain.statistics().peak<=total as u64);
        drop(audit);
    }
}

fn fixed_reservation_errors_preserve_classification_without_allocations() {
    use mcap::storage::{BudgetRef,BudgetLimits,ResourceCategory};
    let total=BudgetRef::allocation_size()+4096;
    let domain=BudgetRef::new(BudgetLimits {total,block:4096,retained:0}).unwrap();
    let occupied=domain.reserve(4096).unwrap();
    for (requested,temporary) in [(1,true),(total+1,false)] {
        let mut response=Response::default();
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=guard(&mut response,|_| {
            let error:Error=domain.try_reserve(requested,ResourceCategory::Scratch).err().unwrap().into();
            assert_eq!(budget::unavailable(&error),temporary);
            assert_eq!(budget::requested_capacity(&error),Some(requested));
            Err(error)
        });
        assert_eq!(COUNTS.with(|c|c.replace(None).unwrap()),(0,0));assert_eq!(status,-1);
        assert_eq!(domain.statistics().current,total as u64);
    }
    drop(occupied);assert_eq!(domain.workload_statistics().current,0);
}

fn linear_input_capacity_errors_have_exact_allocation_identities() {
    use mcap::storage::{BudgetRef, BudgetLimits};
    for temporary in [false, true] {
        let domain = BudgetRef::new(BudgetLimits {
            total: BudgetRef::allocation_size() + 32768, block: 8192, retained: 0,
        }).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit = identity::start(domain.as_ptr() as usize);
        let mut reader = sans_io::LinearReader::new_with_options_and_budget(Default::default(), domain.clone());
        COUNTS.with(|c| c.set(Some((0, 0))));
        reader.try_insert(128).unwrap().fill(7);
        reader.notify_read(128);
        let occupied = if temporary {
            Some(domain.reserve(32768 - domain.workload_statistics().current as usize).unwrap())
        } else { None };
        let mut response = Response::default();
        let status = guard(&mut response, |_| {
            let error: Error = reader.try_insert(if temporary { 4096 } else { 8192 }).err().unwrap().into();
            assert_eq!(budget::unavailable(&error), temporary);
            Err(error)
        });
        assert_eq!(status, -1);
        drop(occupied);
        assert_eq!(reader.try_insert(4096).unwrap().len(), 4096);
        drop(reader);
        let calls = COUNTS.with(|c| c.replace(None).unwrap());
        let actual = identity::snapshot();
        assert_eq!(calls, (actual.bound, actual.bound_bytes));
        assert_eq!(actual.errors, 0);
        assert_eq!(actual.live, [0; 9]);
        assert_eq!(domain.workload_statistics().current, 0);
        drop(audit);
    }
}

fn retained_topic_filters_have_exact_allocation_identities() {
    use mcap::storage::{BudgetRef,BudgetLimits};
    let setup=BudgetRef::new(Default::default()).unwrap();
    let document=budget_json::Document::parse(br#"["topic-a","topic-z","topic-a",null,7]"#,&setup).unwrap();
    for fail in [None,Some(0),Some(1),Some(2),Some(3),Some(4),Some(5)] {
        let domain=BudgetRef::new(BudgetLimits {retained:0,..Default::default()}).unwrap();
        domain.observe_allocations(identity::claimed);
        let audit=identity::start(domain.as_ptr() as usize);
        if let Some(index)=fail { domain.fail_allocation_at(index); }
        let mut response=Response::default();
        COUNTS.with(|c|c.set(Some((0,0))));
        let _=guard(&mut response,|_| {
            let topics=topic_filter::Topics::parse(document.view(),&domain)?.unwrap();
            assert!(topics.contains("topic-a")); assert!(topics.contains("topic-z"));
            Ok(0)
        });
        let calls=COUNTS.with(|c|c.replace(None).unwrap());
        let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes));
        assert_eq!(actual.errors,0); assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],0);
        drop(audit);
    }
}

fn copied_buffer_constructor_has_exact_allocation_identities() {
    let mut response=Response::default(); let mut handle=ptr::null_mut(); let mut id=0;
    unsafe { assert_eq!(budget::fm_budget_open(1024*1024,1024*1024,0,&mut handle,&mut id,&mut response),0); }
    let domain=budget::resolve(id).unwrap();
    let config=format!(r#"{{"Budget":{{"id":{id}}}}}"#);
    let baseline=domain.statistics().current;
    domain.observe_allocations(identity::claimed);
    let mut allocations=0;
    for iteration in 0..64 {
        if iteration>0 && iteration>allocations { break; }
        domain.fail_allocation_at(if iteration==0 {usize::MAX} else {iteration-1});
        let mut reader=ptr::null_mut();
        let audit=identity::start(domain.as_ptr() as usize);
        COUNTS.with(|c|c.set(Some((0,0))));
        let status=unsafe { buffer_reader::fm_buffer_reader_open_options(0,false,b"buffer".as_ptr(),6,
            config.as_ptr(),config.len(),&mut reader,&mut response) };
        unsafe { buffer_reader::fm_buffer_reader_free(reader); }
        let calls=COUNTS.with(|c|c.replace(None).unwrap()); let actual=identity::snapshot();
        assert_eq!(calls,(actual.bound,actual.bound_bytes),"failure={iteration}");
        assert_eq!(actual.errors,0); assert_eq!(actual.live,[0;9]);
        assert_eq!(domain.statistics().current,baseline);
        if iteration==0 { assert_eq!(status,0); allocations=actual.bound as usize; assert!(allocations>4 && allocations<64); }
        else { assert_eq!(status,-1); assert!(reader.is_null()); }
        drop(audit);
    }
    unsafe { budget::fm_budget_free(handle); }
    assert_eq!(domain.workload_statistics().current,0);
}
