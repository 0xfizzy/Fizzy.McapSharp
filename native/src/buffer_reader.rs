use super::*;
use binrw::BinWrite;
use std::sync::Arc;

pub(super) fn encode(record: records::Record<'_>) -> Outcome<(u8, Vec<u8>)> {
    let op = record.opcode();
    let mut out = std::io::Cursor::new(Vec::new());
    use records::Record::*;
    use std::io::Write;
    match record {
        Header(v) => v.write_le(&mut out)?,
        Footer(v) => v.write_le(&mut out)?,
        Channel(v) => v.write_le(&mut out)?,
        MessageIndex(v) => v.write_le(&mut out)?,
        ChunkIndex(v) => v.write_le(&mut out)?,
        AttachmentIndex(v) => v.write_le(&mut out)?,
        Statistics(v) => v.write_le(&mut out)?,
        Metadata(v) => v.write_le(&mut out)?,
        MetadataIndex(v) => v.write_le(&mut out)?,
        SummaryOffset(v) => v.write_le(&mut out)?,
        DataEnd(v) => v.write_le(&mut out)?,
        Schema { header, data } => {
            header.write_le(&mut out)?;
            out.write_all(&(data.len() as u32).to_le_bytes())?;
            out.write_all(&data)?;
        }
        Message { header, data } => {
            header.write_le(&mut out)?;
            out.write_all(&data)?;
        }
        Chunk { header, data } => {
            header.write_le(&mut out)?;
            out.write_all(&data)?;
        }
        Attachment { header, data, crc } => {
            header.write_le(&mut out)?;
            out.write_all(&(data.len() as u64).to_le_bytes())?;
            out.write_all(&data)?;
            out.write_all(&crc.to_le_bytes())?;
        }
        Unknown { data, .. } => out.write_all(&data)?,
    }
    Ok((op, out.into_inner()))
}
pub struct BufferReader {
    records: VecDeque<Outcome<(u8, Vec<u8>)>>,
    channels: BTreeMap<u16, Arc<mcap::Channel<'static>>>,
    failed: bool,
}
pub(super) fn summary_records(s: &mcap::Summary) -> Outcome<BufferReader> {
    let mut h = BufferReader {
        records: VecDeque::new(),
        channels: BTreeMap::new(),
        failed: false,
    };
    for c in s.channels.values() {
        h.channels.insert(c.id, own_channel(c));
        h.records
            .push_back(encode(records::Record::Channel(records::Channel {
                id: c.id,
                schema_id: c.schema.as_ref().map(|s| s.id).unwrap_or(0),
                topic: c.topic.clone(),
                message_encoding: c.message_encoding.clone(),
                metadata: c.metadata.clone(),
            })));
    }
    for s in s.schemas.values() {
        h.records.push_back(encode(records::Record::Schema {
            header: records::SchemaHeader {
                id: s.id,
                name: s.name.clone(),
                encoding: s.encoding.clone(),
            },
            data: Cow::Borrowed(&s.data),
        }));
    }
    if let Some(v) = &s.stats {
        h.records
            .push_back(encode(records::Record::Statistics(v.clone())));
    }
    for v in &s.chunk_indexes {
        h.records
            .push_back(encode(records::Record::ChunkIndex(v.clone())));
    }
    for v in &s.attachment_indexes {
        h.records
            .push_back(encode(records::Record::AttachmentIndex(v.clone())));
    }
    for v in &s.metadata_indexes {
        h.records
            .push_back(encode(records::Record::MetadataIndex(v.clone())));
    }
    Ok(h)
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_open(
    mode: u32,
    ignore_end: bool,
    p: *const u8,
    n: usize,
    handle: *mut *mut BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let data = bytes(p, n)?;
        let options = if ignore_end {
            enumset::enum_set!(mcap::read::Options::IgnoreEndMagic)
        } else {
            enumset::EnumSet::new()
        };
        let mut h = BufferReader {
            records: VecDeque::new(),
            channels: BTreeMap::new(),
            failed: false,
        };
        match mode {
            0 | 1 | 2 | 3 => {
                let iter: Box<dyn Iterator<Item = mcap::McapResult<records::Record<'_>>> + '_> =
                    match mode {
                        0 => Box::new(mcap::read::LinearReader::new_with_options(data, options)?),
                        1 => Box::new(mcap::read::LinearReader::sans_magic(data)),
                        2 => Box::new(mcap::read::ChunkFlattener::new_with_options(data, options)?),
                        _ => {
                            let records::Record::Chunk { header, data: body } =
                                mcap::parse_record(records::op::CHUNK, data)?
                            else {
                                unreachable!()
                            };
                            let Cow::Borrowed(body) = body else {
                                unreachable!()
                            };
                            Box::new(mcap::read::ChunkReader::new(header, body)?)
                        }
                    };
                for record in iter {
                    match record {
                        Ok(r) => h.records.push_back(encode(r.into_owned())),
                        Err(e) => {
                            h.records.push_back(Err(e.into()));
                            break;
                        }
                    }
                }
            }
            4 => {
                let mut iter = mcap::read::RawMessageStream::new_with_options(data, options)?;
                while let Some(message) = iter.next() {
                    match message {
                        Ok(m) => {
                            if let Some(c) = iter.get_channel(m.header.channel_id) {
                                h.channels.insert(c.id, own_channel(&c));
                            }
                            h.records.push_back(encode(records::Record::Message {
                                header: m.header,
                                data: m.data,
                            }));
                        }
                        Err(e) => {
                            h.records.push_back(Err(e.into()));
                            break;
                        }
                    }
                }
                // Upstream exposes lookup, not channel iteration. Query the bounded ID
                // space after traversal to retain every successfully encountered declaration,
                // including channels with no messages and declarations before an error.
                for id in 0..=u16::MAX {
                    if let Some(c) = iter.get_channel(id) {
                        h.channels.entry(id).or_insert_with(|| own_channel(&c));
                    }
                }
            }
            5 => {
                for message in mcap::MessageStream::new_with_options(data, options)? {
                    match message {
                        Ok(m) => {
                            h.channels.insert(m.channel.id, own_channel(&m.channel));
                            h.records.push_back(encode(records::Record::Message {
                                header: records::MessageHeader {
                                    channel_id: m.channel.id,
                                    sequence: m.sequence,
                                    log_time: m.log_time,
                                    publish_time: m.publish_time,
                                },
                                data: m.data,
                            }));
                        }
                        Err(e) => {
                            h.records.push_back(Err(e.into()));
                            break;
                        }
                    }
                }
            }
            _ => return Err("Unknown buffer reader mode".into()),
        }
        *handle = Box::into_raw(Box::new(h));
        Ok(0)
    })
}
fn own_channel(c: &mcap::Channel<'_>) -> Arc<mcap::Channel<'static>> {
    Arc::new(mcap::Channel {
        id: c.id,
        topic: c.topic.clone(),
        message_encoding: c.message_encoding.clone(),
        metadata: c.metadata.clone(),
        schema: c.schema.as_ref().map(|s| {
            Arc::new(mcap::Schema {
                id: s.id,
                name: s.name.clone(),
                encoding: s.encoding.clone(),
                data: Cow::Owned(s.data.to_vec()),
            })
        }),
    })
}
pub(super) fn describe_channel(c: &mcap::Channel<'_>, out: &mut Response) -> Outcome<i32> {
    let schema = c
        .schema
        .as_ref()
        .map(|s| json!({"id":s.id,"name":s.name,"encoding":s.encoding}));
    respond(
        out,
        serde_json::to_vec(
            &json!({"id":c.id,"topic":c.topic,"messageEncoding":c.message_encoding,"metadata":c.metadata,"schema":schema}),
        )?,
        c.schema
            .as_ref()
            .map(|s| s.data.to_vec())
            .unwrap_or_default(),
        0,
    );
    Ok(0)
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_channel(
    p: *const BufferReader,
    id: u16,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null reader")?;
        describe_channel(
            h.channels
                .get(&id)
                .ok_or(mcap::McapError::UnknownChannel(0, id))?,
            out,
        )
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_next(
    p: *mut BufferReader,
    dest: *mut u8,
    capacity: usize,
    opcode: *mut u8,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |out| {
        if !opcode.is_null() {
            *opcode = 0;
        }
        let h = p.as_mut().ok_or("Null reader")?;
        if h.failed {
            return Err("Reader failed".into());
        }
        if h.records.front().is_some_and(|r| r.is_err()) {
            return Err(h.records.pop_front().unwrap().unwrap_err());
        }
        let Some(Ok((op, data))) = h.records.front() else {
            return Ok(1);
        };
        if !opcode.is_null() {
            *opcode = *op;
        }
        let status = extended::copy_body(data, dest, capacity, out)?;
        if status == 0 {
            h.records.pop_front();
        }
        Ok(status)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
        }
    }
    status
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_free(p: *mut BufferReader) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(p))));
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_message(
    p: *mut BufferReader,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |out| {
        let h = p.as_mut().ok_or("Null reader")?;
        if header.is_null() {
            return Err("Null header".into());
        }
        *header = MessageHeader::default();
        if h.failed {
            return Err("Reader failed".into());
        }
        if h.records.front().is_some_and(|r| r.is_err()) {
            return Err(h.records.pop_front().unwrap().unwrap_err());
        }
        let Some(Ok((op, data))) = h.records.front() else {
            return Ok(1);
        };
        let records::Record::Message { header: m, data } = mcap::parse_record(*op, data)? else {
            return Err("Message reader required".into());
        };
        *header = MessageHeader {
            channel_id: m.channel_id,
            sequence: m.sequence,
            log_time: m.log_time,
            publish_time: m.publish_time,
            reserved: 0,
        };
        let status = extended::copy_body(&data, dest, capacity, out)?;
        if status == 0 {
            h.records.pop_front();
        }
        Ok(status)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
        }
    }
    status
}
