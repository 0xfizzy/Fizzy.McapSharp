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
