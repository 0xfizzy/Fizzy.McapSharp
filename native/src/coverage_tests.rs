use super::*;

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
                extended::fm_snapshot_bytes(
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
                extended::fm_snapshot_call(
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
                extended::fm_snapshot_call(
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
            extended::fm_snapshot_free(snapshot);
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
