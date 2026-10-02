//! Independent, unmodified registry parser for classifying accepted Lz4 timeouts.
use std::io::{Cursor, Read};

fn main() {
    let data = std::fs::read(std::env::args().nth(1).expect("input path")).unwrap();
    let mut input = Cursor::new(data);
    let mut reader = mcap::sans_io::LinearReader::new_with_options(
        mcap::sans_io::LinearReaderOptions::default().with_validate_chunk_crcs(true),
    );
    while let Some(event) = reader.next_event() {
        match event {
            Err(_) => return,
            Ok(mcap::sans_io::LinearReadEvent::Record { .. }) => {}
            Ok(mcap::sans_io::LinearReadEvent::ReadRequest(n)) => {
                let written = input.read(reader.insert(n)).unwrap();
                reader.notify_read(written);
            }
        }
    }
}
