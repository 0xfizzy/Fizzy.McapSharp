//! Independent registry parser. Only operations with equivalent options are eligible.
#[path = "../probe_protocol.rs"]
mod probe_protocol;
use probe_protocol as trace;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let mode: u32 = args[2].parse().unwrap();
    assert!([0, 2].contains(&mode), "Unsupported reference operation");
    trace::start(mode, data.len());
    trace::emit("strict open");
    let mut options = mcap::sans_io::LinearReaderOptions::default()
        .with_validate_chunk_crcs(true).with_emit_chunks(mode == 0);
    if mode == 0 { options = options.with_record_length_limit(data.len()); }
    let mut reader = mcap::sans_io::LinearReader::new_with_options(options);
    let mut supplied = false;
    let mut ordinal = 0;
    let mut hash = trace::INITIAL_HASH;
    loop {
        trace::next(mode, ordinal, hash);
        // One FFI next call includes input supply, parsing and record validation.
        // Supply the entire remaining input, as the binding's slice reader does.
        loop {
            match reader.next_event() {
                None | Some(Err(_)) => { trace::emit("done"); return; }
                Some(Ok(mcap::sans_io::LinearReadEvent::ReadRequest(_))) => {
                    if supplied { reader.notify_read(0); }
                    else {
                        reader.insert(data.len()).copy_from_slice(&data);
                        reader.notify_read(data.len());
                        supplied = true;
                    }
                }
                Some(Ok(mcap::sans_io::LinearReadEvent::Record { opcode, data })) => {
                    if mcap::parse_record(opcode, data).is_err() || data.len() > trace::CAPACITY {
                        trace::emit("done"); return;
                    }
                    trace::observe(&mut hash, opcode, data);
                    ordinal += 1;
                    break;
                }
            }
        }
    }
}
