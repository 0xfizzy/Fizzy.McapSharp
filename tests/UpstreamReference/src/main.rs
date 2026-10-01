// Compiled independently against registry mcap and the locally patched crate.
use std::{collections::BTreeMap, hash::{Hash, Hasher}, io::Cursor};
fn fingerprint(value: impl std::fmt::Debug) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{value:?}").hash(&mut h); h.finish()
}
fn scan(data: &[u8]) -> u64 {
    fingerprint(mcap::read::MessageStream::new(data).map(|s| s.collect::<Vec<_>>()))
}
fn main() {
    for compression in [None, Some(mcap::Compression::Lz4), Some(mcap::Compression::Zstd)] {
        for chunks in [false, true] {
            let mut writer = mcap::WriteOptions::new().compression(compression).use_chunks(chunks)
                .chunk_size(Some(128)).create(Cursor::new(Vec::new())).unwrap();
            let channel = writer.add_channel(0, "topic", "raw", &BTreeMap::new()).unwrap();
            for sequence in 0..12 {
                writer.write_to_known_channel(&mcap::records::MessageHeader {
                    channel_id: channel, sequence, log_time: (12 - sequence) as u64, publish_time: 5
                }, &vec![sequence as u8; sequence as usize * 17]).unwrap();
            }
            writer.finish().unwrap();
            let data = writer.into_inner().into_inner();
            println!("fixture {} {}", data.len(), fingerprint(&data));
            println!("messages {}", scan(&data));
            println!("records {}", fingerprint(mcap::read::LinearReader::new(&data).unwrap().collect::<Vec<_>>()));
            if let Some(summary) = mcap::Summary::read(&data).unwrap() {
                let mut reader = mcap::sans_io::IndexedReader::new(&summary).unwrap();
                let mut indexed = Vec::new();
                while let Some(event) = reader.next_event() {
                    match event.unwrap() {
                        mcap::sans_io::IndexedReadEvent::ReadChunkRequest { offset, length } => {
                            reader.insert_chunk_record_data(offset, &data[offset as usize..offset as usize + length]).unwrap();
                        }
                        mcap::sans_io::IndexedReadEvent::Message { header, data } => indexed.push(fingerprint((header, data))),
                    }
                }
                println!("sansio-indexed {indexed:?}");

                for index in &summary.chunk_indexes {
                    println!("chunk {}", fingerprint(summary.stream_chunk(&data, index).unwrap().collect::<Vec<_>>()));
                    let indexes = summary.read_message_indexes(&data, index).unwrap();
                    let mut messages = Vec::new();
                    for entries in indexes.values() { for entry in entries {
                        messages.push(fingerprint(summary.seek_message(&data, index, entry)));
                    }}
                    messages.sort(); println!("indexed {messages:?}");
                }
            }
            for length in 0..data.len() { println!("truncated {length} {}", scan(&data[..length])); }
            for position in [0, 7, data.len() - 1] {
                let mut corrupt = data.clone(); corrupt[position] ^= 0xff;
                println!("corrupt {position} {}", scan(&corrupt));
            }
        }
    }
}
