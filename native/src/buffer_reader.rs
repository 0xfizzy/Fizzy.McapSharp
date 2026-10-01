use super::*;
use binrw::BinWrite;
use std::sync::Arc;

pub(super) fn encode(record: records::Record<'_>) -> Outcome<(u8, Vec<u8>)> {
    let mut out = std::io::Cursor::new(Vec::new());
    write_record(&record, &mut out)?;
    Ok((record.opcode(), out.into_inner()))
}
fn write_record<W: std::io::Write + std::io::Seek>(
    record: &records::Record<'_>,
    mut out: &mut W,
) -> Outcome<()> {
    use records::Record::*;
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
    Ok(())
}
pub struct BufferReader {
    pub delivery: memory::Delivery,
    summary_position: usize,
    summary_keys: Vec<u16>,
    summary_only: bool,
    parser: Option<sans_io::LinearReader>,
    pub input: Arc<memory::Backing>,
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
            delivery: memory::Delivery::default(),
            summary_position: 0,
            summary_keys: Vec::new(),
            summary_only: false,
            parser: None,
            input: Arc::new(memory::Backing::empty()),
            position: 0,
            end: 0,
            mode: 0,
            summary: None,
            schemas: BTreeMap::new(),
            channels: BTreeMap::new(),
            failed: false,
        }
    }
    pub(super) fn observe(
        schemas: &mut BTreeMap<u16, Arc<mcap::Schema<'static>>>,
        channels: &mut BTreeMap<u16, Arc<mcap::Channel<'static>>>,
        record: records::Record<'_>,
        _delivery: &mut memory::Delivery,
    ) -> Outcome<()> {
        match record {
            records::Record::Schema { header, data } => {
                if header.id == 0 {
                    return Err(mcap::McapError::InvalidSchemaId.into());
                }
                if let Some(old) = schemas.get(&header.id) {
                    if old.name != header.name
                        || old.encoding != header.encoding
                        || old.data != data
                    {
                        return Err(mcap::McapError::ConflictingSchemas(header.name.clone()).into());
                    }
                } else {
                    schemas.insert(
                        header.id,
                        Arc::new(mcap::Schema {
                            id: header.id,
                            name: header.name,
                            encoding: header.encoding,
                            data: Cow::Owned(data.into_owned()),
                        }),
                    );
                }
            }
            records::Record::Channel(c) => {
                let schema = if c.schema_id == 0 {
                    None
                } else {
                    Some(
                        schemas
                            .get(&c.schema_id)
                            .ok_or_else(|| {
                                mcap::McapError::UnknownSchema(c.topic.clone(), c.schema_id)
                            })?
                            .clone(),
                    )
                };
                if let Some(old) = channels.get(&c.id) {
                    if old.topic != c.topic
                        || old.message_encoding != c.message_encoding
                        || old.metadata != c.metadata
                        || old.schema.as_ref().map(|s| s.id).unwrap_or(0) != c.schema_id
                    {
                        return Err(mcap::McapError::ConflictingChannels(c.topic.clone()).into());
                    }
                } else {
                    let mut size = c
                        .topic
                        .len()
                        .checked_add(c.message_encoding.len())
                        .and_then(|n| n.checked_add(1024))
                        .ok_or("Channel capacity overflow")?;
                    for (k, v) in &c.metadata {
                        size = size
                            .checked_add(k.len())
                            .and_then(|n| n.checked_add(v.len()))
                            .and_then(|n| n.checked_add(256))
                            .ok_or("Channel capacity overflow")?;
                    }
                    channels.insert(
                        c.id,
                        Arc::new(mcap::Channel {
                            id: c.id,
                            topic: c.topic,
                            message_encoding: c.message_encoding,
                            metadata: c.metadata,
                            schema,
                        }),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
    unsafe fn read(
        &mut self,
        dest: *mut u8,
        capacity: usize,
        message: bool,
        out: &mut Response,
    ) -> Outcome<i32> {
        if self.failed {
            return Err("Reader failed".into());
        }
        if self.delivery.active && self.delivery.capture {
            let n = self.delivery.bytes().len();
            let start = if message { 22 } else { 0 };
            if message && self.delivery.opcode != records::op::MESSAGE {
                return Err("Message reader required".into());
            }
            self.delivery.shared = Some(self.delivery.take_shared(start)?);
            self.delivery.active = false;
            out.value = (n - start) as u64;
            return Ok(0);
        }
        if self.delivery.active {
            let body = self.delivery.bytes();
            let payload = if message {
                if self.delivery.opcode != records::op::MESSAGE {
                    return Err("Message reader required".into());
                }
                // The original record was validated and its header cached before becoming pending.
                Cow::Borrowed(&body[22..])
            } else {
                Cow::Borrowed(body)
            };
            out.value = payload.len() as u64;
            if let Some(sink) = self.delivery.sink {
                sink.send(self.delivery.opcode, &self.delivery.header, &payload)?;
            } else {
                if capacity < payload.len() {
                    return Ok(2);
                }
                memory::copy(&payload, dest)?;
            }
            self.delivery.release();
            return Ok(0);
        }
        if self.summary_only {
            if message {
                return Err("Message reader required".into());
            }
            let summary = self.summary.as_ref().unwrap().clone();
            let Some(record) =
                Self::summary_record(&summary, &self.summary_keys, self.summary_position)
            else {
                return Ok(1);
            };
            let mut measure = memory::Measure::default();
            write_record(&record, &mut measure)?;
            let n = usize::try_from(measure.length)?;
            self.delivery.opcode = record.opcode();
            out.value = n as u64;
            if capacity >= n {
                if n != 0 && dest.is_null() {
                    return Err("Null destination".into());
                }
                let output = if n == 0 {
                    &mut []
                } else {
                    slice::from_raw_parts_mut(dest, n)
                };
                write_record(&record, &mut std::io::Cursor::new(output))?;
                self.summary_position += 1;
                self.delivery.release();
                return Ok(0);
            }
            self.delivery.data.clear();
            self.delivery.reserve(n)?;
            write_record(&record, &mut std::io::Cursor::new(&mut self.delivery.data))?;
            self.summary_position += 1;
            self.delivery.active = true;
            if self.delivery.sink.is_some() {
                return self.delivery.retry(dest, capacity);
            }
            return Ok(2);
        }
        loop {
            let Some(parser) = self.parser.as_mut() else {
                return Ok(1);
            };
            match parser.next_shared_event().transpose()? {
                None => {
                    self.parser = None;
                    return Ok(1);
                }
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                    let _ = n;
                    if self.position == self.end {
                        parser.notify_read(0);
                    } else {
                        parser.supply_shared(mcap::storage::SharedBytes::external(
                            self.input.clone(),
                            self.position..self.end,
                        ));
                        self.position = self.end;
                    }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record {
                    opcode,
                    data: shared,
                }) => {
                    let data = shared.as_ref();
                    let record = mcap::parse_record(opcode, data)?;
                    if self.mode >= 4 {
                        if !matches!(record, records::Record::Message { .. }) {
                            if self.summary.is_none() {
                                Self::observe(
                                    &mut self.schemas,
                                    &mut self.channels,
                                    record,
                                    &mut self.delivery,
                                )?;
                            }
                            continue;
                        }
                        let records::Record::Message { header, .. } = &record else {
                            unreachable!()
                        };
                        if self.mode != 4
                            && !self
                                .summary
                                .as_ref()
                                .map(|s| s.channels.contains_key(&header.channel_id))
                                .unwrap_or_else(|| self.channels.contains_key(&header.channel_id))
                        {
                            return Err(mcap::McapError::UnknownChannel(
                                header.sequence,
                                header.channel_id,
                            )
                            .into());
                        }
                    }
                    self.delivery.opcode = opcode;
                    if let records::Record::Message { header, .. } = &record {
                        self.delivery.header = native_header(header);
                    }
                    let payload = if message {
                        let records::Record::Message { header, data } = &record else {
                            return Err("Message reader required".into());
                        };
                        self.delivery.header = native_header(header);
                        data.as_ref()
                    } else {
                        data
                    };
                    out.value = payload.len() as u64;
                    self.delivery.shared = Some(shared.clone());
                    if self.delivery.capture {
                        if message {
                            self.delivery.shared = Some(shared.slice(22..data.len()));
                        }
                        return Ok(0);
                    }
                    if let Some(sink) = self.delivery.sink {
                        sink.send(opcode, &self.delivery.header, payload)?;
                        self.delivery.release();
                        return Ok(0);
                    }
                    if capacity < payload.len() {
                        // Keep the original body so record/message retries may be interchanged.
                        self.delivery.deliver(data, ptr::null_mut(), 0)?;
                        return Ok(2);
                    }
                    memory::copy(payload, dest)?;
                    self.delivery.release();
                    return Ok(0);
                }
            }
        }
    }
    fn summary_record<'a>(
        s: &'a mcap::Summary,
        keys: &[u16],
        position: usize,
    ) -> Option<records::Record<'a>> {
        let mut n = position;
        if n < s.channels.len() {
            let c = &s.channels[&keys[n]];
            return Some(records::Record::Channel(records::Channel {
                id: c.id,
                schema_id: c.schema.as_ref().map(|s| s.id).unwrap_or(0),
                topic: c.topic.clone(),
                message_encoding: c.message_encoding.clone(),
                metadata: c.metadata.clone(),
            }));
        }
        n -= s.channels.len();
        if n < s.schemas.len() {
            let v = &s.schemas[&keys[s.channels.len() + n]];
            return Some(records::Record::Schema {
                header: records::SchemaHeader {
                    id: v.id,
                    name: v.name.clone(),
                    encoding: v.encoding.clone(),
                },
                data: Cow::Borrowed(&v.data),
            });
        }
        n -= s.schemas.len();
        if let Some(v) = &s.stats {
            if n == 0 {
                return Some(records::Record::Statistics(v.clone()));
            }
            n -= 1;
        }
        if n < s.chunk_indexes.len() {
            return Some(records::Record::ChunkIndex(s.chunk_indexes[n].clone()));
        }
        n -= s.chunk_indexes.len();
        if n < s.attachment_indexes.len() {
            return Some(records::Record::AttachmentIndex(
                s.attachment_indexes[n].clone(),
            ));
        }
        n -= s.attachment_indexes.len();
        s.metadata_indexes
            .get(n)
            .map(|v| records::Record::MetadataIndex(v.clone()))
    }
}
pub(super) fn native_header(h: &records::MessageHeader) -> MessageHeader {
    MessageHeader {
        channel_id: h.channel_id,
        sequence: h.sequence,
        log_time: h.log_time,
        publish_time: h.publish_time,
        reserved: 0,
    }
}
// for_chunk is private upstream. Feed a synthetic record prefix into the public
// parser, then stream the original body without copying or retaining an iterator.
pub(super) fn chunk_parser(
    header: records::ChunkHeader,
    data: &[u8],
    length: usize,
) -> Outcome<sans_io::LinearReader> {
    // Match ChunkReader construction errors (notably unsupported compression).
    if !matches!(header.compression.as_str(), "" | "lz4" | "zstd") {
        return Err(mcap::McapError::UnsupportedCompression(header.compression).into());
    }
    let _ = data;
    let mut parser = sans_io::LinearReader::new_with_options(
        sans_io::LinearReaderOptions::default()
            .with_skip_start_magic(true)
            .with_skip_end_magic(true)
            .with_validate_chunk_crcs(true),
    );
    let mut prefix = [0u8; 9];
    prefix[0] = records::op::CHUNK;
    prefix[1..].copy_from_slice(&(length as u64).to_le_bytes());
    parser.try_insert(9)?.copy_from_slice(&prefix);
    parser.notify_read(9);
    Ok(parser)
}
pub(super) fn chunk_reader(
    input: Arc<memory::Backing>,
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
pub(super) fn summary_records(s: Arc<mcap::Summary>) -> Outcome<BufferReader> {
    let mut h = BufferReader::empty();
    h.summary_keys.extend(s.channels.keys().copied());
    h.summary_keys.extend(s.schemas.keys().copied());
    h.summary = Some(s);
    h.summary_only = true;
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
        let input = memory::Backing::copy(bytes(p, n)?)?;
        *handle = Box::into_raw(Box::new(open_backing(input, mode, ignore_end)?));
        Ok(0)
    })
}
fn open_backing(data: memory::Backing, mode: u32, ignore_end: bool) -> Outcome<BufferReader> {
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
            mcap::parse_record(records::op::CHUNK, &data)?
        else {
            unreachable!()
        };
        (chunk_parser(header, &body, data.len())?, 0)
    } else {
        (sans_io::LinearReader::new_with_options(options), 0)
    };
    let end = data.len();
    let h = BufferReader {
        delivery: memory::Delivery::default(),
        input: Arc::new(data),
        position,
        end,
        parser: Some(parser),
        mode,
        ..BufferReader::empty()
    };
    Ok(h)
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_mapped(
    config: *const u8,
    n: usize,
    handle: *mut *mut BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let v = request(config, n)?;
        let mode = u32::try_from(v["mode"].as_u64().ok_or("Invalid mode")?)?;
        let input = memory::Backing::open(string(&v, "path")?)?;
        *handle = Box::into_raw(Box::new(open_backing(
            input,
            mode,
            v["ignoreEndMagic"].as_bool().unwrap_or(false),
        )?));
        Ok(0)
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
        let status = h.read(dest, capacity, false, out)?;
        if !opcode.is_null() {
            *opcode = if status == 1 { 0 } else { h.delivery.opcode };
        }
        Ok(status)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.parser = None;
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
        let status = h.read(dest, capacity, true, out)?;
        *header = if status == 1 {
            MessageHeader::default()
        } else {
            h.delivery.header
        };
        Ok(status)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.parser = None;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_owned(
    p: *mut BufferReader,
    message: bool,
    sink: memory::Sink,
    out: *mut Response,
) -> i32 {
    if let Some(h) = p.as_mut() {
        h.delivery.sink = Some(sink);
    }
    let status = if message {
        fm_buffer_reader_message(p, ptr::null_mut(), 0, &mut MessageHeader::default(), out)
    } else {
        fm_buffer_reader_next(p, ptr::null_mut(), 0, ptr::null_mut(), out)
    };
    if let Some(h) = p.as_mut() {
        h.delivery.sink = None;
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
                    assert!(!(*h).delivery.active);
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
                            assert_eq!((*h).delivery.active, true);
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
                        assert!(!(*h).delivery.active);
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
                    assert!(!(*h).delivery.active);
                    eprintln!(
                        "mode {mode}, excluding input: pending capacity {} bytes, declarations {}",
                        (*h).delivery.data.capacity(),
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
