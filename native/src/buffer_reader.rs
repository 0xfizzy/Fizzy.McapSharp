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
    parser: Option<sans_io::LinearReader>,
    input: Arc<Vec<u8>>,
    position: usize,
    end: usize,
    mode: u32,
    summary: Option<Arc<mcap::Summary>>,
    schemas: BTreeMap<u16, Arc<mcap::Schema<'static>>>,
    channels: BTreeMap<u16, Arc<mcap::Channel<'static>>>,
    failed: bool,
}
impl BufferReader {
    fn empty() -> Self {
        Self {
            records: VecDeque::new(),
            parser: None,
            input: Arc::new(Vec::new()),
            position: 0,
            end: 0,
            mode: 0,
            summary: None,
            schemas: BTreeMap::new(),
            channels: BTreeMap::new(),
            failed: false,
        }
    }
    fn observe(&mut self, record: &records::Record<'_>) -> Outcome<()> {
        match record {
            records::Record::Schema { header, data } => {
                if header.id == 0 {
                    return Err(mcap::McapError::InvalidSchemaId.into());
                }
                if let Some(old) = self.schemas.get(&header.id) {
                    if old.name != header.name
                        || old.encoding != header.encoding
                        || old.data != *data
                    {
                        return Err(mcap::McapError::ConflictingSchemas(header.name.clone()).into());
                    }
                } else {
                    self.schemas.insert(
                        header.id,
                        Arc::new(mcap::Schema {
                            id: header.id,
                            name: header.name.clone(),
                            encoding: header.encoding.clone(),
                            data: Cow::Owned(data.to_vec()),
                        }),
                    );
                }
            }
            records::Record::Channel(c) => {
                let schema = if c.schema_id == 0 {
                    None
                } else {
                    Some(
                        self.schemas
                            .get(&c.schema_id)
                            .ok_or_else(|| {
                                mcap::McapError::UnknownSchema(c.topic.clone(), c.schema_id)
                            })?
                            .clone(),
                    )
                };
                if let Some(old) = self.channels.get(&c.id) {
                    if old.topic != c.topic
                        || old.message_encoding != c.message_encoding
                        || old.metadata != c.metadata
                        || old.schema.as_ref().map(|s| s.id).unwrap_or(0) != c.schema_id
                    {
                        return Err(mcap::McapError::ConflictingChannels(c.topic.clone()).into());
                    }
                } else {
                    self.channels.insert(
                        c.id,
                        Arc::new(mcap::Channel {
                            id: c.id,
                            topic: c.topic.clone(),
                            message_encoding: c.message_encoding.clone(),
                            metadata: c.metadata.clone(),
                            schema,
                        }),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn advance(&mut self) -> Outcome<()> {
        if self.failed {
            return Err("Reader failed".into());
        }
        if !self.records.is_empty() {
            return Ok(());
        }
        loop {
            let Some(parser) = self.parser.as_mut() else {
                return Ok(());
            };
            let next = match parser.next_event().transpose()? {
                None => {
                    self.parser = None;
                    return Ok(());
                }
                Some(sans_io::LinearReadEvent::ReadRequest(n)) => {
                    let n = n.min(self.end - self.position);
                    parser
                        .insert(n)
                        .copy_from_slice(&self.input[self.position..self.position + n]);
                    parser.notify_read(n);
                    self.position += n;
                    continue;
                }
                Some(sans_io::LinearReadEvent::Record { opcode, data }) => {
                    mcap::parse_record(opcode, data)?.into_owned()
                }
            };
            if self.mode >= 4 {
                if self.summary.is_none() {
                    self.observe(&next)?;
                }
                let records::Record::Message { header, .. } = &next else {
                    continue;
                };
                if self.mode != 4 {
                    let known = self
                        .summary
                        .as_ref()
                        .map(|s| s.channels.contains_key(&header.channel_id))
                        .unwrap_or_else(|| self.channels.contains_key(&header.channel_id));
                    if !known {
                        return Err(mcap::McapError::UnknownChannel(
                            header.sequence,
                            header.channel_id,
                        )
                        .into());
                    }
                }
            }
            self.records.push_back(encode(next));
            return Ok(());
        }
    }
}
// for_chunk is private upstream. Feed a synthetic record prefix into the public
// parser, then stream the original body without copying or retaining an iterator.
fn chunk_parser(
    header: records::ChunkHeader,
    data: &[u8],
    length: usize,
) -> Outcome<sans_io::LinearReader> {
    // Match ChunkReader construction errors (notably unsupported compression).
    let _ = mcap::read::ChunkReader::new(header, data)?;
    let mut parser = sans_io::LinearReader::new_with_options(
        sans_io::LinearReaderOptions::default()
            .with_skip_start_magic(true)
            .with_skip_end_magic(true)
            .with_validate_chunk_crcs(true),
    );
    let mut prefix = [0u8; 9];
    prefix[0] = records::op::CHUNK;
    prefix[1..].copy_from_slice(&(length as u64).to_le_bytes());
    parser.insert(9).copy_from_slice(&prefix);
    parser.notify_read(9);
    Ok(parser)
}
pub(super) fn chunk_reader(
    input: Arc<Vec<u8>>,
    summary: Arc<mcap::Summary>,
    index: &records::ChunkIndex,
) -> Outcome<BufferReader> {
    let start = usize::try_from(
        index
            .chunk_start_offset
            .checked_add(9)
            .ok_or(mcap::McapError::BadIndex)?,
    )?;
    let end = usize::try_from(
        index
            .chunk_start_offset
            .checked_add(index.chunk_length)
            .ok_or(mcap::McapError::BadIndex)?,
    )?;
    let body = input.get(start..end).ok_or(mcap::McapError::BadIndex)?;
    let records::Record::Chunk { header, data } = mcap::parse_record(records::op::CHUNK, body)?
    else {
        unreachable!()
    };
    let parser = chunk_parser(header, &data, body.len())?;
    let position = start;
    Ok(BufferReader {
        input,
        position,
        end,
        parser: Some(parser),
        summary: Some(summary),
        mode: 5,
        ..BufferReader::empty()
    })
}
pub(super) fn summary_records(s: &mcap::Summary) -> Outcome<BufferReader> {
    let mut h = BufferReader::empty();
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
        if mode > 5 {
            return Err("Unknown buffer reader mode".into());
        }
        let mut options = sans_io::LinearReaderOptions::default();
        if mode == 1 {
            options = options
                .with_record_length_limit(data.len())
                .with_skip_start_magic(true)
                .with_skip_end_magic(true);
        } else {
            options = options
                .with_skip_end_magic(ignore_end)
                .with_validate_chunk_crcs(true)
                .with_emit_chunks(mode == 0);
            if mode == 0 {
                options = options.with_record_length_limit(data.len());
            }
        }
        let (parser, position) = if mode == 3 {
            let records::Record::Chunk { header, data: body } =
                mcap::parse_record(records::op::CHUNK, data)?
            else {
                unreachable!()
            };
            (chunk_parser(header, &body, data.len())?, 0)
        } else {
            (sans_io::LinearReader::new_with_options(options), 0)
        };
        let h = BufferReader {
            input: Arc::new(data.to_vec()),
            position,
            end: data.len(),
            parser: Some(parser),
            mode,
            ..BufferReader::empty()
        };
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
            h.summary
                .as_ref()
                .and_then(|s| s.channels.get(&id))
                .or_else(|| h.channels.get(&id))
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
        h.advance()?;
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
        h.advance()?;
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

#[cfg(test)]
mod lazy_tests {
    use super::*;
    fn fixture(compression: Option<mcap::Compression>) -> Vec<u8> {
        let mut w = mcap::WriteOptions::new()
            .compression(compression)
            .chunk_size(Some(1000))
            .create(std::io::Cursor::new(Vec::new()))
            .unwrap();
        let c = w.add_channel(0, "topic", "raw", &BTreeMap::new()).unwrap();
        for sequence in 0..200 {
            w.write_to_known_channel(
                &records::MessageHeader {
                    channel_id: c,
                    sequence,
                    log_time: sequence as u64,
                    publish_time: 0,
                },
                &[42; 128],
            )
            .unwrap();
        }
        w.finish().unwrap();
        w.into_inner().into_inner()
    }
    fn expected(data: &[u8], mode: u32) -> Vec<(u8, Vec<u8>)> {
        let iter: Box<dyn Iterator<Item = mcap::McapResult<records::Record<'_>>> + '_> = match mode
        {
            0 => Box::new(mcap::read::LinearReader::new(data).unwrap()),
            1 => Box::new(mcap::read::LinearReader::sans_magic(data)),
            2 => Box::new(mcap::read::ChunkFlattener::new(data).unwrap()),
            3 => {
                let records::Record::Chunk { header, data } = mcap::parse_record(6, data).unwrap()
                else {
                    unreachable!()
                };
                let Cow::Borrowed(data) = data else {
                    unreachable!()
                };
                Box::new(mcap::read::ChunkReader::new(header, data).unwrap())
            }
            4 => Box::new(mcap::read::RawMessageStream::new(data).unwrap().map(|r| {
                r.map(|m| records::Record::Message {
                    header: m.header,
                    data: m.data,
                })
            })),
            _ => Box::new(mcap::MessageStream::new(data).unwrap().map(|r| {
                r.map(|m| records::Record::Message {
                    header: records::MessageHeader {
                        channel_id: m.channel.id,
                        sequence: m.sequence,
                        log_time: m.log_time,
                        publish_time: m.publish_time,
                    },
                    data: m.data,
                })
            })),
        };
        iter.map(|r| encode(r.unwrap()).unwrap()).collect()
    }
    #[test]
    fn all_modes_match_official_and_retain_only_one_pending_record() {
        for compression in [
            None,
            Some(mcap::Compression::Lz4),
            Some(mcap::Compression::Zstd),
        ] {
            let file = fixture(compression);
            let chunk = expected(&file, 0).into_iter().find(|r| r.0 == 6).unwrap().1;
            for mode in 0..6 {
                let input = match mode {
                    1 => &file[8..file.len() - 8],
                    3 => &chunk,
                    _ => &file,
                };
                let expected = expected(input, mode);
                unsafe {
                    let mut h = ptr::null_mut();
                    let mut response = Response::default();
                    assert_eq!(
                        fm_buffer_reader_open(
                            mode,
                            false,
                            input.as_ptr(),
                            input.len(),
                            &mut h,
                            &mut response
                        ),
                        0
                    );
                    assert_eq!((*h).position, 0);
                    assert!((*h).records.is_empty());
                    let input_capacity = (*h).input.capacity();
                    let mut output = vec![0u8; 65536];
                    let mut opcode = 0;
                    for (op, body) in &expected {
                        if !body.is_empty() {
                            assert_eq!(
                                fm_buffer_reader_next(
                                    h,
                                    ptr::null_mut(),
                                    0,
                                    &mut opcode,
                                    &mut response
                                ),
                                2
                            );
                            assert_eq!((*h).records.len(), 1);
                            assert_eq!(response.value as usize, body.len());
                        }
                        assert_eq!(
                            fm_buffer_reader_next(
                                h,
                                output.as_mut_ptr(),
                                output.len(),
                                &mut opcode,
                                &mut response
                            ),
                            0
                        );
                        assert_eq!(opcode, *op);
                        assert_eq!(&output[..response.value as usize], body);
                        assert!((*h).records.is_empty());
                        assert_eq!((*h).input.capacity(), input_capacity);
                    }
                    assert_eq!(
                        fm_buffer_reader_next(
                            h,
                            output.as_mut_ptr(),
                            output.len(),
                            &mut opcode,
                            &mut response
                        ),
                        1
                    );
                    assert!((*h).parser.is_none());
                    assert!((*h).records.is_empty());
                    eprintln!(
                        "mode {mode}, excluding input: queue capacity {} slots, declarations {}",
                        (*h).records.capacity(),
                        (*h).channels.len()
                    );
                    fm_buffer_reader_free(h);
                }
            }
        }
    }
    #[test]
    fn malformed_inputs_match_official_error_and_valid_prefix() {
        let file = fixture(None);
        let mut bad_magic = file.clone();
        bad_magic[0] = 0;
        let mut bad_chunk = file.clone();
        let mut offset = 8;
        while bad_chunk[offset] != records::op::CHUNK {
            offset += 9 + u64::from_le_bytes(bad_chunk[offset + 1..offset + 9].try_into().unwrap())
                as usize;
        }
        bad_chunk[offset + 9 + 24] ^= 1; // uncompressed CRC
        let truncated = file[..file.len() - 12].to_vec();
        for data in [bad_magic, bad_chunk, truncated] {
            let mut official = mcap::read::RawMessageStream::new(&data).unwrap();
            unsafe {
                let mut h = ptr::null_mut();
                let mut r = Response::default();
                let mut header = MessageHeader::default();
                assert_eq!(
                    fm_buffer_reader_open(4, false, data.as_ptr(), data.len(), &mut h, &mut r),
                    0
                );
                let mut output = [0u8; 256];
                loop {
                    let status = fm_buffer_reader_message(
                        h,
                        output.as_mut_ptr(),
                        output.len(),
                        &mut header,
                        &mut r,
                    );
                    match official.next() {
                        Some(Ok(m)) => {
                            assert_eq!(status, 0);
                            assert_eq!(header.sequence, m.header.sequence);
                            assert_eq!(&output[..r.value as usize], m.data.as_ref());
                        }
                        Some(Err(e)) => {
                            assert_eq!(status, -1);
                            assert_eq!(bytes(r.json, r.json_len).unwrap(), errors::encode(&e));
                            fm_buffer_free(r.json, r.json_len);
                            fm_buffer_free(r.data, r.data_len);
                            break;
                        }
                        None => {
                            assert_eq!(status, 1);
                            break;
                        }
                    }
                }
                fm_buffer_reader_free(h);
            }
        }
    }
    #[test]
    fn corrupted_tail_is_not_parsed_on_construction_or_drop() {
        let mut data = fixture(None);
        data[0] = 0;
        unsafe {
            let mut h = ptr::null_mut();
            let mut r = Response::default();
            assert_eq!(
                fm_buffer_reader_open(5, false, data.as_ptr(), data.len(), &mut h, &mut r),
                0
            );
            assert_eq!((*h).position, 0);
            fm_buffer_reader_free(h);
        }
    }
}
