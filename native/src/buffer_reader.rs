use super::*;
use binrw::BinWrite;

#[cfg(test)]
pub(super) fn encode_chunk(index: &mcap::shared_chunk_index::SharedChunkIndex) -> Outcome<(u8, Vec<u8>)> {
    let mut out = std::io::Cursor::new(Vec::new());
    index.write_le(&mut out)?;
    Ok((records::op::CHUNK_INDEX, out.into_inner()))
}
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
pub(super) type SchemaTable = mcap::u16_table::U16Table<mcap::shared_declarations::SharedSchema>;
pub(super) type ChannelTable = mcap::u16_table::U16Table<mcap::shared_declarations::SharedChannel>;
pub struct BufferReader {
    pub delivery: memory::Delivery,
    summary_position: usize,
    summary_keys: mcap::segmented::BudgetedSegmentedVec<u16>,
    summary_only: bool,
    parser: Option<sans_io::LinearReader>,
    pub input: memory::Source,
    position: usize,
    end: usize,
    mode: u32,
    summary: Option<SharedSummary>,
    schemas: SchemaTable,
    channels: ChannelTable,
    failed: bool,
}
impl BufferReader {
    pub(super) fn into_handle(self) -> Outcome<*mut Self> {
        let domain=self.delivery.options.domain.clone();
        let owner=mcap::charged::ChargedBox::new_fixed(self,&domain,mcap::storage::ResourceCategory::Scratch)?;
        owner.charge_owner(mcap::storage::OwnerKind::Parser,true);
        Ok(owner.into_raw_value())
    }
    pub(super) fn empty(options: memory::Options) -> Self {
        Self {
            delivery: memory::Delivery::new(options.clone()),
            summary_position: 0,
            summary_keys: mcap::segmented::BudgetedSegmentedVec::new(options.domain.clone(),mcap::storage::ResourceCategory::Index),
            summary_only: false,
            parser: None,
            input: memory::Source::empty(),
            position: 0,
            end: 0,
            mode: 0,
            summary: None,
            schemas: SchemaTable::new_owned(options.domain.clone(), mcap::storage::ResourceCategory::Declaration, mcap::storage::OwnerKind::Parser),
            channels: ChannelTable::new_owned(options.domain.clone(), mcap::storage::ResourceCategory::Declaration, mcap::storage::OwnerKind::Parser),
            failed: false,
        }
    }
    pub(super) fn observe(
        schemas: &mut SchemaTable,
        channels: &mut ChannelTable,
        opcode: u8,
        data: &[u8],
        delivery: &memory::Delivery,
    ) -> Outcome<()> {
        use mcap::{shared_declarations::{SharedSchema, SharedChannel}, storage::OwnerKind};
        let domain = &delivery.options.domain;
        match opcode {
            records::op::SCHEMA => {
                let schema = SharedSchema::read(data, domain, OwnerKind::Parser)?;
                if schema.id == 0 { return Err(mcap::McapError::InvalidSchemaId.into()); }
                if let Some(old) = schemas.get(&schema.id) {
                    if old != &schema {
                        return Err(mcap::McapError::ConflictingSchemas(schema.name.clone()).into());
                    }
                } else { schemas.insert_fixed(schema.id, schema)?; }
            }
            records::op::CHANNEL => {
                let channel = SharedChannel::read_with_schema_lookup(
                    data, |id| schemas.get(&id).cloned(), domain, OwnerKind::Parser,
                )?;
                if let Some(old) = channels.get(&channel.id) {
                    if old != &channel {
                        return Err(mcap::McapError::ConflictingChannels(channel.topic.clone()).into());
                    }
                } else { channels.insert_fixed(channel.id, channel)?; }
            }
            _ => { record_input::validate(opcode, data, domain)?; }
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
        if self.delivery.active {
            if message && self.delivery.opcode != records::op::MESSAGE {
                return Err("Message reader required".into());
            }
            let start = if message { 22 } else { 0 };
            out.value = self.delivery.pending_len().checked_sub(start).ok_or("Invalid pending message body")? as u64;
            return self.delivery.retry_range(start, dest, capacity);
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
            record.write_body(&mut measure)?;
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
                record.write_body(&mut std::io::Cursor::new(output))?;
                self.delivery.record_delivery(n);
                self.summary_position += 1;
                self.delivery.release();
                return Ok(0);
            }
            self.delivery.data.clear();
            self.delivery.reserve(n)?;
            record.write_body(&mut std::io::Cursor::new(&mut self.delivery.data))?;
            self.delivery.stats.copied += n as u64;
            self.delivery.options.domain.copy_bytes(mcap::storage::CopyKind::Other,n);
            self.summary_position += 1;
            self.delivery.active = true;
            if self.delivery.sink.is_some() { return self.delivery.retry(dest, capacity); }
            return Ok(2);
        }
        loop {
            let Some(parser) = self.parser.as_mut() else {
                return Ok(1);
            };
            parser.set_memory_budget(self.delivery.options.domain.clone())?;
            match parser.next_shared_event().transpose()? {
                None => {
                    self.parser = None;
                    return Ok(1);
                }
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                    let _ = n;
                    if self.position == self.end { parser.notify_read(0); }
                    else { parser.supply_shared(self.input.shared(self.position..self.end)); self.position = self.end; }
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data: shared }) => {
                    let data = shared.as_ref();
                    if self.mode >= 4 && self.summary.is_none() && opcode != records::op::MESSAGE {
                        Self::observe(&mut self.schemas, &mut self.channels, opcode, data, &self.delivery).map_err(budget::after_advance)?;
                        continue;
                    }
                    record_input::validate(opcode, data, &self.delivery.options.domain)?;
                    let header = if opcode == records::op::MESSAGE {
                        let records::Record::Message {header,..} = mcap::parse_record(opcode,data)? else {unreachable!()};
                        Some(header)
                    } else {None};
                    if self.mode >= 4 {
                        let Some(header) = &header else {continue;};
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
                    if let Some(header) = &header {
                        self.delivery.header = native_header(header);
                    }
                    let payload = if message {
                        let Some(header) = &header else {
                            return Err("Message reader required".into());
                        };
                        self.delivery.header = native_header(header);
                        &data[22..]
                    } else {
                        data
                    };
                    out.value = payload.len() as u64;
                    self.delivery.shared=Some(shared.slice(if message { 22..data.len() } else { 0..data.len() }));
                    if self.delivery.capture { return Ok(0); }
                    if let Some(sink) = self.delivery.sink {
                        let copied = sink.send(opcode, &self.delivery.header, payload)?;
                        self.delivery.record_delivery(copied as usize);
                        self.delivery.release();
                        return Ok(0);
                    }
                    if capacity < payload.len() {
                        // Keep the original body so record/message retries may be interchanged.
                        self.delivery.shared = Some(shared);
                        self.delivery.retain_pending(self.delivery.pending_len())?;
                        return Ok(2);
                    }
                    memory::copy(payload, dest)?;
                    self.delivery.record_delivery(payload.len());
                    self.delivery.release();
                    return Ok(0);
                }
            }
        }
    }
    fn summary_record<'a>(
        s: &'a mcap::Summary,
        keys: &mcap::segmented::BudgetedSegmentedVec<u16>,
        position: usize,
    ) -> Option<records::SummaryRecordRef<'a>> {
        use records::SummaryRecordRef as View;
        let mut n = position;
        if n < s.channels.len() {
            return Some(View::Channel(&s.channels[&keys[n]]));
        }
        n -= s.channels.len();
        if n < s.schemas.len() {
            return Some(View::Schema(&s.schemas[&keys[s.channels.len() + n]]));
        }
        n -= s.schemas.len();
        if let Some(v) = &s.stats {
            if n == 0 { return Some(View::Statistics(v)); }
            n -= 1;
        }
        if n < s.chunk_indexes.len() { return Some(View::ChunkIndex(&s.chunk_indexes[n])); }
        n -= s.chunk_indexes.len();
        if n < s.attachment_indexes.len() { return Some(View::AttachmentIndex(&s.attachment_indexes[n])); }
        n -= s.attachment_indexes.len();
        s.metadata_indexes.get(n).map(|index| View::MetadataIndex(index))
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
    header: records::BorrowedChunkHeader<'_>,
    data: &[u8],
    length: usize,
    domain: mcap::storage::BudgetRef,
) -> Outcome<sans_io::LinearReader> {
    // Match ChunkReader construction errors (notably unsupported compression).
    if !matches!(header.compression, "" | "lz4" | "zstd") {
        return Err(mcap::McapError::UnsupportedCompression(header.compression.into()).into());
    }
    let _ = data;
    let mut parser = sans_io::LinearReader::new_with_options_and_budget(
        sans_io::LinearReaderOptions::default()
            .with_skip_start_magic(true)
            .with_skip_end_magic(true)
            .with_validate_chunk_crcs(true),
        domain,
    );
    let mut prefix = [0u8; 9];
    prefix[0] = records::op::CHUNK;
    prefix[1..].copy_from_slice(&(length as u64).to_le_bytes());
    parser.try_insert(9)?.copy_from_slice(&prefix);
    parser.notify_read(9);
    Ok(parser)
}
pub(super) fn chunk_reader(
    input: memory::Source,
    summary: SharedSummary,
    index: &mcap::shared_chunk_index::SharedChunkIndex,
    options: memory::Options,
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
    let (header, data) = mcap::read::parse_borrowed_chunk(body)?;
    let parser = chunk_parser(header, &data, body.len(), options.domain.clone())?;
    let position = start;
    Ok(BufferReader {
        input,
        position,
        end,
        parser: Some(parser),
        summary: Some(summary),
        mode: 5,
        ..BufferReader::empty(options)
    })
}
pub(super) fn summary_records(s: SharedSummary, options: memory::Options) -> Outcome<BufferReader> {
    let mut h = BufferReader::empty(options);
    h.summary_keys.reserve(s.channels.len()+s.schemas.len())?;
    h.summary_keys.owner_reference(mcap::storage::OwnerKind::Parser,true);
    for key in s.channels.keys().chain(s.schemas.keys()) { h.summary_keys.push(key)?; }
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
    fm_buffer_reader_open_options(mode, ignore_end, p, n, ptr::null(), 0, handle, out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_open_options(
    mode: u32,
    ignore_end: bool,
    p: *const u8,
    n: usize,
    config: *const u8,
    config_len: usize,
    handle: *mut *mut BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let memory_options = if config_len == 0 {
            memory::Options::try_default()?
        } else {
            memory::Options::parse_config(bytes(config, config_len)?)?
        };
        let input = memory::Backing::copy(bytes(p, n)?, memory_options.clone())?;
        *handle = open_backing(input, mode, ignore_end, memory_options)?.into_handle()?;
        Ok(0)
    })
}
fn open_backing(data: memory::Backing, mode: u32, ignore_end: bool,
    memory_options: memory::Options) -> Outcome<BufferReader> {
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
        let (header, body) = mcap::read::parse_borrowed_chunk(&data)?;
        (chunk_parser(header, &body, data.len(), memory_options.domain.clone())?, 0)
    } else {
        (sans_io::LinearReader::new_with_options_and_budget(options, memory_options.domain.clone()), 0)
    };
    let end = data.len();
    let h = BufferReader {
        input: memory::Source::new(data, &memory_options.domain)?,
        position,
        end,
        parser: Some(parser),
        mode,
        ..BufferReader::empty(memory_options)
    };
    Ok(h)
}
#[no_mangle]
pub unsafe extern "C" fn fm_buffer_reader_mapped(config: *const u8, n: usize,
    handle: *mut *mut BufferReader, out: *mut Response) -> i32 {
    guard(out, |_| {
        if handle.is_null() { return Err("Null output".into()); }
        *handle = ptr::null_mut();
        let (document, domain) = budget_json::Document::configured(bytes(config,n)?, &["options","Budget","id"])?;
        let v = document.view();
        let mode = u32::try_from(v.get("mode").as_u64().ok_or("Invalid mode")?)?;
        let options = memory::Options::parse_view(v.get("options"),domain)?;
        let input = memory::Backing::open(budget_json::Control::Charged(v).string("path")?, &options.domain)?;
        let ignore_end = v.get("ignoreEndMagic").as_bool().unwrap_or(false);
        drop(document);
        *handle = open_backing(input, mode, ignore_end, options)?.into_handle()?;
        Ok(0)
    })
}

pub(super) fn describe_shared_channel(c: &mcap::shared_declarations::SharedChannel, out: &mut Response, domain: &mcap::storage::BudgetRef) -> Outcome<i32> {
    response::channel(out, domain, c, true)?;
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
        if let Some(channel) = h.summary.as_ref().and_then(|s| s.channels.get(&id)) {
            describe_shared_channel(channel, out, &h.delivery.options.domain)
        } else {
            describe_shared_channel(h.channels.get(&id).ok_or(mcap::McapError::UnknownChannel(0,id))?, out, &h.delivery.options.domain)
        }
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
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<BufferReader>::from_raw_value(p))));
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
        let status = match h.read(dest, capacity, true, out) { Err(ref e) if h.delivery.capture && budget::unavailable(e)=>{
            h.delivery.retry_capacity=budget::requested_capacity(e).ok_or("Missing retry capacity")?;
            return Ok(4);
        }, other=>other? };
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
    p: *mut BufferReader, message: bool, sink: memory::Sink, out: *mut Response,
) -> i32 {
    if let Some(h) = p.as_mut() { h.delivery.sink = Some(sink); }
    let status = if message {
        fm_buffer_reader_message(p, ptr::null_mut(), 0, &mut MessageHeader::default(), out)
    } else {
        fm_buffer_reader_next(p, ptr::null_mut(), 0, ptr::null_mut(), out)
    };
    if let Some(h) = p.as_mut() { h.delivery.sink = None; }
    status
}


#[cfg(test)]
mod lazy_tests {
    use std::borrow::Cow;
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
                            assert_eq!(r.error.as_bytes(), errors::encode(&e));
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

#[cfg(test)]
mod root_accounting_tests {
    use super::*;
    #[test]
    fn summary_key_pages_preserve_order_and_roll_back_on_refusal() {
        let mut summary=mcap::Summary::default();
        let declarations=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        for id in 0..40000u16 {
            summary.schemas.insert_fixed(id,mcap::shared_declarations::SharedSchema::new(id,"","",&[],&declarations,mcap::storage::OwnerKind::Parser).unwrap()).unwrap();
        }
        let summary=retain_summary(summary, &declarations, mcap::storage::OwnerKind::Parser).unwrap();
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let cursor=summary_records(summary.clone(),memory::Options { domain:domain.clone(), ..Default::default() }).unwrap();
        assert_eq!(cursor.summary_keys.len(),40000);
        for i in 0..40000 { assert_eq!(cursor.summary_keys[i],i as u16); }
        let records::SummaryRecordRef::Schema(header)=BufferReader::summary_record(&summary,&cursor.summary_keys,32768).unwrap() else { panic!("Expected schema"); };
        assert_eq!(header.id,32768);
        let peak=domain.workload_statistics().peak as usize;
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],domain.workload_statistics().current);
        drop(cursor);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        let limited=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {total:(peak-1) + mcap::storage::BudgetRef::allocation_size(),block:peak-1,retained:0}).unwrap();
        assert!(summary_records(summary,memory::Options { domain:limited.clone(), ..Default::default() }).is_err());
        assert_eq!(limited.workload_statistics().current,0);
    }

    #[test]
    fn cursor_root_refusal_does_not_publish_or_leak() {
        let domain=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits { total:(1) + mcap::storage::BudgetRef::allocation_size(), block:1, retained:0 }).unwrap();
        let cursor=BufferReader::empty(memory::Options { domain:domain.clone(), ..Default::default() });
        assert!(cursor.into_handle().is_err());
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
    #[test]
    fn cursor_root_stays_charged_until_matching_free() {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let cursor=BufferReader::empty(memory::Options { domain:domain.clone(), ..Default::default() });
        let handle=cursor.into_handle().unwrap();
        let capacity=domain.workload_statistics().current;
        assert!(capacity>=std::mem::size_of::<BufferReader>() as u64);
        assert_eq!(domain.ownership_statistics().bytes[mcap::storage::OwnerKind::Parser as usize],capacity);
        unsafe { fm_buffer_reader_free(handle); }
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
}

#[cfg(test)]
mod declaration_budget_tests {
    use super::*;
    #[test]
    fn consumed_declaration_refusal_is_terminal_even_in_capture_mode() {
        let mut writer=mcap::WriteOptions::new().use_chunks(false).create(std::io::Cursor::new(Vec::new())).unwrap();
        let schema=writer.add_schema("schema","raw",&[7;4096]).unwrap();
        let channel=writer.add_channel(schema,"topic","raw",&BTreeMap::new()).unwrap();
        writer.write_to_known_channel(&records::MessageHeader {channel_id:channel,sequence:0,log_time:0,publish_time:0},&[1]).unwrap();
        writer.finish().unwrap();
        let data=writer.into_inner().into_inner();
        let domain=mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {total:(1024*1024) + mcap::storage::BudgetRef::allocation_size(),retained:0,..Default::default()}).unwrap();
        let options=memory::Options {domain:domain.clone(),..Default::default()};
        let backing=memory::Backing::copy(&data,options.clone()).unwrap();
        let mut reader=open_backing(backing,5,false,options).unwrap();
        reader.delivery.capture=true;
        let occupied=domain.reserve(1024*1024-domain.workload_statistics().current as usize-16).unwrap();
        let mut header=MessageHeader::default();
        let mut response=Response::default();
        unsafe {
            let status=fm_buffer_reader_message(&mut reader,ptr::null_mut(),0,&mut header,&mut response);
            assert!(status<0,"consumed declaration was reported as retryable: {status}");
            assert!(reader.failed);
            fm_buffer_free(response.json,response.json_len);
            fm_buffer_free(response.data,response.data_len);
            drop(occupied);
            assert!(fm_buffer_reader_message(&mut reader,ptr::null_mut(),0,&mut header,&mut response)<0);
            fm_buffer_free(response.json,response.json_len);
            fm_buffer_free(response.data,response.data_len);
        }
        drop(reader);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
}
