use super::io::Output;
use std::fs::File;
use super::writer::{
    options, Writer, writer_control, writer_guard, registration_result, fm_writer_open,
    fm_writer_message, fm_writer_call,
};
use super::{
    buffer_reader, engine, errors, io, snapshot, bytes, fm_buffer_free, Response,
    MessageHeader,
};
use std::collections::BTreeMap;
use std::{ptr, slice};
use mcap::records;
use serde_json::{json, Value};
use super::io::Callbacks;

#[test]
fn private_abi_layout_matches_managed_contract() {
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<Response>(), 40);
    assert_eq!(
        [
            offset_of!(Response, json),
            offset_of!(Response, json_len),
            offset_of!(Response, data),
            offset_of!(Response, data_len),
            offset_of!(Response, value)
        ],
        [0, 8, 16, 24, 32]
    );
    assert_eq!(size_of::<MessageHeader>(), 24);
    assert_eq!(
        [
            offset_of!(MessageHeader, channel_id),
            offset_of!(MessageHeader, reserved),
            offset_of!(MessageHeader, sequence),
            offset_of!(MessageHeader, log_time),
            offset_of!(MessageHeader, publish_time)
        ],
        [0, 2, 4, 8, 16]
    );
    assert_eq!(size_of::<Callbacks>(), 48);
    assert_eq!(
        [
            offset_of!(Callbacks, context),
            offset_of!(Callbacks, read),
            offset_of!(Callbacks, write),
            offset_of!(Callbacks, seek),
            offset_of!(Callbacks, flush),
            offset_of!(Callbacks, seekable)
        ],
        [0, 8, 16, 24, 32, 40]
    );
    assert_eq!(size_of::<engine::Event>(), 56);
}

fn fixture(options: mcap::WriteOptions) -> Vec<u8> {
    let mut w = options.create(std::io::Cursor::new(Vec::new())).unwrap();
    let schema = w.add_schema_with_id(7, "schema", "raw", &[1, 2]).unwrap();
    let channel = w
        .add_channel_with_id(9, schema, "topic", "raw", &BTreeMap::new())
        .unwrap();
    for sequence in 0..3 {
        w.write_to_known_channel(
            &records::MessageHeader {
                channel_id: channel,
                sequence,
                log_time: 3 - sequence as u64,
                publish_time: 0,
            },
            &[42; 700000],
        )
        .unwrap();
    }
    w.finish().unwrap();
    w.into_inner().into_inner()
}

#[test]
fn defaults_match_official_records() {
    let upstream = fixture(mcap::WriteOptions::new());
    let wrapper = fixture(
        options(
            &json!({"compression":"zstd","chunkSize":1048576,"useChunks":true,"profile":""}),
            true,
        )
        .unwrap(),
    );
    let describe = |data: &[u8]| {
        mcap::read::LinearReader::new(data)
            .unwrap()
            .map(|r| match r.unwrap() {
                records::Record::Chunk { header, .. } => format!(
                    "{}:{}:{}",
                    header.message_start_time, header.message_end_time, header.uncompressed_size
                ),
                r => format!("{r:?}"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(describe(&upstream), describe(&wrapper));
}

#[test]
fn option_mapping_matches_explicit_upstream_configuration() {
    let upstream = mcap::WriteOptions::new()
        .compression(None)
        .chunk_size(None)
        .use_chunks(false)
        .profile("p")
        .library("l")
        .disable_seeking(true)
        .emit_summary_records(false)
        .emit_statistics(true)
        .emit_summary_offsets(false)
        .calculate_data_section_crc(false)
        .calculate_summary_section_crc(false)
        .calculate_chunk_crcs(false)
        .calculate_attachment_crcs(false)
        .compression_threads(0)
        .compression_level(0);
    let wrapper = options(&json!({"compression":"none","chunkSize":null,"useChunks":false,"profile":"p","library":"l","disableSeeking":true,"emitSummaryRecords":false,"emitStatistics":true,"emitSummaryOffsets":false,"calculateDataSectionCrc":false,"calculateSummarySectionCrc":false,"calculateChunkCrcs":false,"calculateAttachmentCrcs":false,"compressionThreads":0,"compressionLevel":0}),true).unwrap();
    assert_eq!(fixture(upstream), fixture(wrapper));
    assert!(options(
        &json!({"compression":"none","profile":"","disableSeeking":false}),
        false
    )
    .is_err());
}

#[test]
fn structured_errors_preserve_fields() {
    let e = mcap::McapError::BadChunkCrc {
        saved: 12,
        calculated: 34,
    };
    let v: Value = serde_json::from_slice(&errors::encode(&e)).unwrap();
    assert_eq!(v["kind"], "BadChunkCrc");
    assert_eq!(v["details"]["saved"], 12);
    assert_eq!(v["details"]["calculated"], 34);
}

#[test]
fn caller_indexes_match_official_helpers() {
    for compression in [
        None,
        Some(mcap::Compression::Lz4),
        Some(mcap::Compression::Zstd),
    ] {
        let data = fixture(mcap::WriteOptions::new().compression(compression));
        let summary = mcap::Summary::read(&data).unwrap().unwrap();
        let mut index = summary.chunk_indexes[0].clone();
        unsafe {
            let mut snapshot = ptr::null_mut();
            let mut response = Response::default();
            assert_eq!(
                snapshot::fm_snapshot_bytes(
                    data.as_ptr(),
                    data.len(),
                    &mut snapshot,
                    &mut response
                ),
                0
            );
            // The chunk start is irrelevant to read_message_indexes. The supplied map
            // is authoritative even when it differs from the summary's stored index.
            index.chunk_start_offset = u64::MAX;
            let expected = summary.read_message_indexes(&data, &index).unwrap();
            let (_, encoded) =
                buffer_reader::encode(records::Record::ChunkIndex(index.clone())).unwrap();
            let mut output = vec![0; 1024];
            let mut header = MessageHeader::default();
            assert_eq!(
                snapshot::fm_snapshot_call(
                    snapshot,
                    5,
                    encoded.as_ptr(),
                    encoded.len(),
                    0,
                    0,
                    output.as_mut_ptr(),
                    output.len(),
                    &mut header,
                    &mut response
                ),
                0
            );
            assert_eq!(
                response.value as usize,
                expected.values().map(|v| v.len() * 18).sum::<usize>()
            );
            index.message_index_offsets.clear();
            let expected_error = summary.read_message_indexes(&data, &index).unwrap_err();
            let (_, encoded) = buffer_reader::encode(records::Record::ChunkIndex(index)).unwrap();
            assert_eq!(
                snapshot::fm_snapshot_call(
                    snapshot,
                    5,
                    encoded.as_ptr(),
                    encoded.len(),
                    0,
                    0,
                    output.as_mut_ptr(),
                    output.len(),
                    &mut header,
                    &mut response
                ),
                -1
            );
            assert_eq!(
                bytes(response.json, response.json_len).unwrap(),
                errors::encode(&expected_error)
            );
            fm_buffer_free(response.json, response.json_len);
            fm_buffer_free(response.data, response.data_len);
            snapshot::fm_snapshot_free(snapshot);
        }
    }
}

#[test]
fn raw_channel_lookup_matches_upstream_after_error() {
    // Preserve a valid declaration preceding a conflicting declaration, with no messages.
    let mut data = mcap::MAGIC.to_vec();
    for topic in ["first", "conflict"] {
        let (opcode, body) = buffer_reader::encode(records::Record::Channel(records::Channel {
            id: u16::MAX,
            schema_id: 0,
            topic: topic.into(),
            message_encoding: "raw".into(),
            metadata: BTreeMap::new(),
        }))
        .unwrap();
        data.push(opcode);
        data.extend_from_slice(&(body.len() as u64).to_le_bytes());
        data.extend_from_slice(&body);
    }
    data.extend_from_slice(mcap::MAGIC);
    let mut official = mcap::read::RawMessageStream::new(&data).unwrap();
    assert!(official.next().unwrap().is_err());
    let expected = official.get_channel(u16::MAX).unwrap();
    unsafe {
        let mut reader = ptr::null_mut();
        let mut response = Response::default();
        assert_eq!(
            buffer_reader::fm_buffer_reader_open(
                4,
                false,
                data.as_ptr(),
                data.len(),
                &mut reader,
                &mut response
            ),
            0
        );
        let mut op = 0;
        assert_eq!(
            buffer_reader::fm_buffer_reader_next(
                reader,
                ptr::null_mut(),
                0,
                &mut op,
                &mut response
            ),
            -1
        );
        fm_buffer_free(response.json, response.json_len);
        fm_buffer_free(response.data, response.data_len);
        assert_eq!(
            buffer_reader::fm_buffer_reader_channel(reader, u16::MAX, &mut response),
            0
        );
        let channel: Value =
            serde_json::from_slice(bytes(response.json, response.json_len).unwrap()).unwrap();
        assert_eq!(channel["topic"], expected.topic);
        fm_buffer_free(response.json, response.json_len);
        fm_buffer_free(response.data, response.data_len);
        buffer_reader::fm_buffer_reader_free(reader);
    }
}

#[test]
fn writer_error_boundary_is_fail_closed() {
    let mut out = Response::default();
    assert_eq!(
        writer_guard(&mut out, |_| {
            registration_result::<()>(Err(mcap::McapError::InvalidSchemaId), 1, true, true)?;
            Ok(0)
        }),
        -2
    );
    unsafe {
        fm_buffer_free(out.json, out.json_len);
    }
    for mode in 0..3 {
        let status = writer_guard(&mut out, |_| {
            if mode == 0 {
                panic!("injected writer panic");
            }
            if mode == 1 {
                return Err("unknown error".into());
            }
            registration_result::<()>(Err(mcap::McapError::InvalidSchemaId), 0, true, true)?;
            Ok(0)
        });
        assert_eq!(status, -1);
        unsafe {
            fm_buffer_free(out.json, out.json_len);
        }
    }
}

#[test]
fn recovered_registration_matches_official_output() {
    for bit in [1u32, 2, 4, 8, 16] {
        let path =
            std::env::temp_dir().join(format!("mcap-recovery-{}-{bit}.mcap", std::process::id()));
        let config = json!({"compression":"none","chunkSize":64,"useChunks":true,"profile":""});
        let mut upstream = options(&config, true)
            .unwrap()
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let mut wrapper = Writer {
            inner: Some(
                options(&config, true)
                    .unwrap()
                    .create(Output::File(File::create(&path).unwrap()))
                    .unwrap(),
            ),
            completed_output: None,
            failed: false,
            recoverable_errors: 31,
            attachment: None,
            native_summary: None,
        };
        let mut out = Response::default();
        upstream.add_schema_with_id(1, "s", "raw", &[]).unwrap();
        upstream
            .add_channel_with_id(1, 1, "t", "raw", &BTreeMap::new())
            .unwrap();
        unsafe {
            writer_control(
                &mut wrapper,
                1,
                &json!({"id":1,"name":"s","encoding":"raw"}),
                &[],
                &mut out,
            )
            .unwrap();
            writer_control(
                &mut wrapper,
                2,
                &json!({"id":1,"schema_id":1,"topic":"t","encoding":"raw","metadata":{}}),
                &[],
                &mut out,
            )
            .unwrap();
        }
        let header = records::MessageHeader {
            channel_id: 2,
            sequence: 99,
            log_time: 999,
            publish_time: 999,
        };
        let error = match bit {
            1 => upstream.add_schema_with_id(0, "s", "raw", &[]).unwrap_err(),
            2 => upstream
                .add_schema_with_id(1, "conflict", "raw", &[])
                .unwrap_err(),
            4 => upstream
                .add_channel_with_id(2, 2, "new", "raw", &BTreeMap::new())
                .unwrap_err(),
            8 => upstream
                .add_channel_with_id(1, 1, "conflict", "raw", &BTreeMap::new())
                .unwrap_err(),
            _ => upstream.write_to_known_channel(&header, &[]).unwrap_err(),
        };
        let status = unsafe {
            if bit == 16 {
                let h = MessageHeader {
                    channel_id: 2,
                    sequence: 99,
                    log_time: 999,
                    publish_time: 999,
                    reserved: 0,
                };
                fm_writer_message(&mut wrapper, &h, ptr::null(), 0, &mut out)
            } else {
                let (op, args) = match bit {
                    1 => (1, json!({"id":0,"name":"s","encoding":"raw"})),
                    2 => (1, json!({"id":1,"name":"conflict","encoding":"raw"})),
                    4 => (
                        2,
                        json!({"id":2,"schema_id":2,"topic":"new","encoding":"raw","metadata":{}}),
                    ),
                    _ => (
                        2,
                        json!({"id":1,"schema_id":1,"topic":"conflict","encoding":"raw","metadata":{}}),
                    ),
                };
                let args = serde_json::to_vec(&args).unwrap();
                fm_writer_call(
                    &mut wrapper,
                    op,
                    args.as_ptr(),
                    args.len(),
                    ptr::null(),
                    0,
                    &mut out,
                )
            }
        };
        assert_eq!(status, -2);
        assert!(!wrapper.failed);
        unsafe {
            assert_eq!(
                slice::from_raw_parts(out.json, out.json_len),
                errors::encode(&error)
            );
            fm_buffer_free(out.json, out.json_len);
        }
        let header = records::MessageHeader {
            channel_id: 1,
            sequence: 1,
            log_time: 10,
            publish_time: 10,
        };
        upstream.write_to_known_channel(&header, &[42]).unwrap();
        wrapper
            .inner
            .as_mut()
            .unwrap()
            .write_to_known_channel(&header, &[42])
            .unwrap();
        upstream.finish().unwrap();
        unsafe {
            writer_control(&mut wrapper, 7, &Value::Null, &[], &mut Response::default()).unwrap();
        }
        drop(wrapper);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            upstream.into_inner().into_inner()
        );
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn native_recovery_configuration_rejects_unknown_bits_before_creation() {
    let path = std::env::temp_dir().join(format!("invalid-recovery-{}.mcap", std::process::id()));
    let req = serde_json::to_vec(&json!({"path":path,"options":{"recoverableErrors":32}})).unwrap();
    let mut out = Response::default();
    let mut handle = ptr::null_mut();
    unsafe {
        assert_eq!(
            fm_writer_open(req.as_ptr(), req.len(), ptr::null(), &mut handle, &mut out),
            -1
        );
        assert!(handle.is_null());
        fm_buffer_free(out.json, out.json_len);
    }
    assert!(!path.exists());
}

#[test]
fn completion_does_not_sync_and_sync_failure_is_terminal() {
    let path = std::env::temp_dir().join(format!("mcap-sync-{}.mcap", std::process::id()));
    let config = json!({"compression":"none","chunkSize":64,"useChunks":true,"profile":""});
    let mut writer = Writer {
        inner: Some(
            options(&config, true)
                .unwrap()
                .create(Output::File(File::create(&path).unwrap()))
                .unwrap(),
        ),
        completed_output: None,
        failed: false,
        recoverable_errors: 31,
        attachment: None,
        native_summary: None,
    };
    io::SYNC_TEST.with(|s| s.set((0, false)));
    unsafe {
        let mut out = Response::default();
        assert_eq!(
            fm_writer_call(&mut writer, 7, ptr::null(), 0, ptr::null(), 0, &mut out),
            0
        );
        assert!(writer.inner.is_none());
        assert!(writer.completed_output.is_some());
        io::SYNC_TEST.with(|s| assert_eq!(s.get().0, 0));
        let before = std::fs::read(&path).unwrap();
        assert!(mcap::Summary::read(&before).unwrap().is_some());
        for _ in 0..2 {
            assert_eq!(
                fm_writer_call(&mut writer, 13, ptr::null(), 0, ptr::null(), 0, &mut out),
                0
            );
        }
        io::SYNC_TEST.with(|s| {
            assert_eq!(s.get().0, 2);
            s.set((2, true));
        });
        assert_eq!(
            fm_writer_call(&mut writer, 13, ptr::null(), 0, ptr::null(), 0, &mut out),
            -1
        );
        assert!(writer.failed);
        assert!(
            String::from_utf8_lossy(bytes(out.json, out.json_len).unwrap())
                .contains("Injected sync failure")
        );
        fm_buffer_free(out.json, out.json_len);
        for op in [7, 12, 13] {
            let mut out = Response::default();
            assert_eq!(
                fm_writer_call(&mut writer, op, ptr::null(), 0, ptr::null(), 0, &mut out),
                -1
            );
            fm_buffer_free(out.json, out.json_len);
        }
        io::SYNC_TEST.with(|s| {
            assert_eq!(s.get().0, 3);
            s.set((0, false));
        });
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    drop(writer);
    std::fs::remove_file(path).unwrap();
}
