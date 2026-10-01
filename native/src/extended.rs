use super::*;

pub(super) fn linear_options(v: &Value) -> Outcome<sans_io::LinearReaderOptions> {
    linear_options_control(budget_json::Control::Existing(v))
}
pub(super) fn linear_options_control(v: budget_json::Control<'_>) -> Outcome<sans_io::LinearReaderOptions> {
    let mut o = sans_io::LinearReaderOptions::default();
    macro_rules! flag {
        ($key:literal, $field:ident) => {
            o.$field = v.get($key).as_bool().unwrap_or(false);
        };
    }
    flag!("SkipStartMagic", skip_start_magic);
    flag!("SkipEndMagic", skip_end_magic);
    flag!("CheckFinishesAfterEndMagic", check_finishes_after_end_magic);
    flag!("EmitChunks", emit_chunks);
    flag!("ValidateChunkCrcs", validate_chunk_crcs);
    flag!("PrevalidateChunkCrcs", prevalidate_chunk_crcs);
    flag!("ValidateDataSectionCrc", validate_data_section_crc);
    flag!("ValidateSummarySectionCrc", validate_summary_section_crc);
    o.record_length_limit = v.get("RecordLengthLimit")
        .as_u64()
        .map(usize::try_from)
        .transpose()?;
    Ok(o)
}

enum Engine {
    Inactive,
    Linear(sans_io::LinearReader),
    Summary(Option<sans_io::SummaryReader>),
    Indexed(sans_io::IndexedReader),
}
#[no_mangle]
pub unsafe extern "C" fn fm_engine_index_control(
    p: *mut EngineHandle,
    op: u32,
    value: u64,
    data: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed {
            return Err("Engine failed".into());
        }
        let Engine::Indexed(r) = &mut h.engine else {
            return Err("Indexed reader required".into());
        };
        match op {
            0 => {
                r.insert_chunk_record_data(value, bytes(data, n)?)?;
                if h.waiting && h.event.kind == 5 && h.event.offset == value {
                    h.waiting = false;
                }
            }
            1 => r.record_length_limit = Some(usize::try_from(value)?),
            2 => r.record_length_limit = None,
            _ => return Err("Unknown indexed operation".into()),
        }
        Ok(0)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}
pub struct EngineHandle {
    capacity_ticket: Option<mcap::storage::CapacityWaitTicket>,
    lease_batch: Option<mcap::charged::ChargedBox<lease::Batch>>,
    schemas: buffer_reader::SchemaTable,
    channels: buffer_reader::ChannelTable,
    engine: Engine,
    event: Event,
    pub delivery: memory::Delivery,
    waiting: bool,
    failed: bool,
    ended: bool,
    summary: Option<SharedSummary>,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Event {
    kind: u32,
    opcode: u32,
    length: u64,
    offset: u64,
    origin: u32,
    reserved: u32,
    header: MessageHeader,
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_open(
    kind: u32,
    p: *const u8,
    n: usize,
    summary: *const EngineHandle,
    handle: *mut *mut EngineHandle,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let input=bytes(p,n)?;
        let inherited=kind==2 && !budget_json::Document::has_non_null(input,&["Memory"])?;
        let (document, options)=if inherited {
            let options=summary.as_ref().ok_or("Missing summary")?.delivery.options.clone();
            (budget_json::Document::parse(input,&options.domain)?,options)
        } else {
            let (document,domain)=budget_json::Document::configured(input,&["Memory","Budget","id"])?;
            let options=memory::Options::parse_view(document.view().get("Memory"),domain)?;
            (document,options)
        };
        let v=document.view();
        let mut engine = match kind {
            0 => Engine::Linear(sans_io::LinearReader::new_with_options_and_budget(linear_options_control(budget_json::Control::Charged(v))?, options.domain.clone())),
            1 => {
                let mut opts = sans_io::SummaryReaderOptions::default();
                if let Some(n) = v.get("FileSize").as_u64() {
                    opts = opts.with_file_size(n);
                }
                if let Some(n) = v.get("RecordLengthLimit").as_u64() {
                    opts = opts.with_record_length_limit(n.try_into()?);
                }
                Engine::Summary(Some(sans_io::SummaryReader::new_with_options_and_budget(opts, options.domain.clone())))
            }
            2 => {
                let s = summary
                    .as_ref()
                    .and_then(|s| s.summary.as_ref())
                    .ok_or("Summary reader must finish with a summary")?;
                let mut opts = sans_io::IndexedReaderOptions::default();
                opts.start = v.get("StartTime").as_u64();
                opts.end = v.get("EndTime").as_u64();
                opts.order = match v.get("Order").as_u64().unwrap_or(0) {
                    0 => sans_io::indexed_reader::ReadOrder::LogTime,
                    1 => sans_io::indexed_reader::ReadOrder::ReverseLogTime,
                    2 => sans_io::indexed_reader::ReadOrder::File,
                    _ => return Err("Invalid read order".into()),
                };
                let topics=topic_filter::Topics::parse(v.get("Topics"),&options.domain)?;
                let topic=v.get("Topic").as_str();
                let filtered=topics.is_some() || topic.is_some();
                let include=filtered.then_some(|name:&str| {
                    if let Some(topics)=&topics {topics.contains(name)} else {topic==Some(name)}
                });
                opts.record_length_limit = v.get("RecordLengthLimit")
                    .as_u64()
                    .map(usize::try_from)
                    .transpose()?;
                Engine::Indexed(sans_io::IndexedReader::new_with_topic_filter(s, opts, options.domain.clone(), include)?)
            }
            _ => return Err("Unknown engine".into()),
        };
        match &mut engine {
            Engine::Linear(r)=>r.set_memory_budget(options.domain.clone())?,
            Engine::Indexed(r)=>r.set_memory_budget(options.domain.clone())?,
            Engine::Summary(Some(r))=>r.set_memory_budget(options.domain.clone())?,
            _=>{},
        }
        let domain=options.domain.clone();
        let root=mcap::charged::ChargedBox::new_fixed(EngineHandle {
            capacity_ticket:None,
            lease_batch:None,
            schemas:buffer_reader::SchemaTable::new_owned(options.domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            channels:buffer_reader::ChannelTable::new_owned(options.domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            engine,
            event: Event::default(),
            delivery: memory::Delivery::new(options),
            waiting: false,
            failed: false,
            ended: false,
            summary: None,
        },&domain,mcap::storage::ResourceCategory::Scratch)?;
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        *handle=root.into_raw_value();
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_next(
    p: *mut EngineHandle,
    dest: *mut u8,
    capacity: usize,
    event: *mut Event,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if event.is_null() {
            return Err("Null event".into());
        }
        if h.failed {
            return Err("Engine failed".into());
        }
        match &mut h.engine {
            Engine::Linear(r) => r.set_memory_budget(h.delivery.options.domain.clone())?,
            Engine::Indexed(r) => r.set_memory_budget(h.delivery.options.domain.clone())?,
            _ => {}
        }
        if !h.waiting && !h.ended {
            h.event = Event::default();
            h.delivery.data.clear();
            match &mut h.engine {
                Engine::Inactive => return Err("Engine failed".into()),
                Engine::Linear(r) => match r.next_shared_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n)) => {
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::linear_reader::SharedReadEvent::Record { opcode, data }) => {
                        h.event.kind = 3;
                        h.event.opcode = opcode as u32;

                        h.event.length = data.as_ref().len() as u64;
                        *event = h.event;
                        h.delivery.shared = Some(data.clone());
                        let status = h.delivery.deliver(data.as_ref(), dest, capacity)?;
                        h.waiting = status == 2;
                        return Ok(status);
                    }
                },
                Engine::Summary(r) => match r
                    .as_mut()
                    .ok_or("Summary consumed")?
                    .next_event()
                    .transpose()?
                {
                    None => {
                        h.summary = r.take().unwrap().finish().map(|s| retain_summary(s, &h.delivery.options.domain, mcap::storage::OwnerKind::Parser)).transpose()?;
                        h.ended = true;
                    }
                    Some(sans_io::SummaryReadEvent::ReadRequest(n)) => {
                        h.event.kind = 1;
                        h.event.length = n as u64;
                    }
                    Some(sans_io::SummaryReadEvent::SeekRequest(s)) => {
                        h.event.kind = 2;
                        (h.event.origin, h.event.offset) = match s {
                            SeekFrom::Start(n) => (0, n),
                            SeekFrom::Current(n) => (1, n as u64),
                            SeekFrom::End(n) => (2, n as u64),
                        };
                    }
                },
                Engine::Indexed(r) => match r.next_shared_event().transpose()? {
                    None => h.ended = true,
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::ReadChunkRequest { offset, length }) => {
                        h.event.kind = 5;
                        h.event.offset = offset;
                        h.event.length = length as u64;
                    }
                    Some(sans_io::indexed_reader::SharedIndexedReadEvent::Message { header, data }) => {
                        h.event.kind = 4;
                        h.event.length = data.as_ref().len() as u64;

                        h.event.header = MessageHeader {
                            channel_id: header.channel_id,
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            reserved: 0,
                        };
                        *event = h.event;
                        h.delivery.shared = Some(data.clone());
                        let status = h.delivery.deliver(data.as_ref(), dest, capacity)?;
                        h.waiting = status == 2;
                        return Ok(status);
                    }
                },
            }
            h.waiting = !h.ended;
        }
        if h.ended {
            h.delivery.discard();
            *event = Event::default();
            return Ok(1);
        }
        *event = h.event;
        if h.event.kind == 3 || h.event.kind == 4 {
            let status = h.delivery.retry(dest, capacity)?;
            if status == 2 {
                return Ok(2);
            }
            h.waiting = false;
        }
        Ok(0)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_feed(
    p: *mut EngineHandle,
    data: *const u8,
    n: usize,
    position: u64,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let h = p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting {
            return Err("No pending input request".into());
        }
        let data = bytes(data, n)?;
        match (&mut h.engine, h.event.kind) {
            (Engine::Linear(r), 1) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.try_insert(n)?.copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), 1) => {
                if n as u64 > h.event.length {
                    return Err("Excess input".into());
                }
                r.try_insert(n)?.copy_from_slice(data);
                r.notify_read(n);
            }
            (Engine::Summary(Some(r)), 2) => r.notify_seeked(position),
            (Engine::Indexed(r), 5) => r.insert_chunk_record_data(position, data)?,
            _ => return Err("Unexpected input".into()),
        }
        h.delivery.stats.copied += n as u64;
        h.waiting = false;
        Ok(0)
    });
    if status < 0 {
        if let Some(h) = p.as_mut() {
            h.failed = true;
            h.delivery.discard();
            h.engine = Engine::Inactive;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_summary(p: *const EngineHandle, out: *mut Response) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null engine")?;
        if !h.ended {
            return Err("Summary scan incomplete".into());
        }
        if let Some(s) = &h.summary {
            summary_response::respond(out, &h.delivery.options.domain, s)?;
        }
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_free(p: *mut EngineHandle) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<EngineHandle>::from_raw_value(p))));
    }
}

pub struct PreparedChannel {
    args: budget_json::Document,
    data: Vec<u8>,
    _charge: mcap::storage::Reservation,
    id: u16,
    schema_id: Option<u16>,
}
impl PreparedChannel {
    pub(super) fn new(input: &[u8], payload: &[u8], domain: &mcap::storage::BudgetRef) -> Outcome<mcap::charged::ChargedBox<Self>> {
        let args = budget_json::Document::parse(input, domain)?;
        let v = budget_json::Control::Charged(args.view());
        let id = v.number("id")?.try_into()?;
        v.string("topic")?;
        v.string("encoding")?;
        v.get("metadata").strings()?;
        let schema = v.get("schema");
        let schema_id = if schema.is_null() { None } else {
            schema.string("name")?;
            schema.string("encoding")?;
            Some(schema.number("id")?.try_into()?)
        };
        let payload = if schema_id.is_some() { payload } else { &[] };
        memory::check(&domain, "StorageBlock", Some(domain.limits().block as u64), payload.len())?;
        let (mut data, charge) = mcap::charged::vector_fixed(domain, mcap::storage::ResourceCategory::Declaration, payload.len())?;
        charge.owner_reference(mcap::storage::OwnerKind::Operation, true);
        data.extend_from_slice(payload);
        domain.copy_bytes(mcap::storage::CopyKind::Other, payload.len());
        let root = mcap::charged::ChargedBox::new_fixed(Self { args, data, _charge: charge, id, schema_id }, domain, mcap::storage::ResourceCategory::Declaration)?;
        root.charge_owner(mcap::storage::OwnerKind::Operation, true);
        Ok(root)
    }
}
pub struct PreparedOperation {
    op: u32,
    args: Option<budget_json::Document>,
    data: Vec<u8>,
    _data_charge: mcap::storage::Reservation,
}
impl PreparedOperation {
    pub(super) fn new(op:u32,args:&[u8],input:&[u8],domain:&mcap::storage::BudgetRef) -> Outcome<mcap::charged::ChargedBox<Self>> {
        memory::check(&domain, "StorageBlock",Some(domain.limits().block as u64),input.len())?;
        let (mut data,charge)=mcap::charged::vector_fixed(domain,mcap::storage::ResourceCategory::Declaration,input.len())?;
        data.extend_from_slice(input);
        domain.copy_bytes(mcap::storage::CopyKind::Other,input.len());
        let root=mcap::charged::ChargedBox::new_fixed(PreparedOperation {
            op,args:None,data,_data_charge:charge,
        },domain,mcap::storage::ResourceCategory::Declaration)?;
        root.charge_owner(mcap::storage::OwnerKind::Operation,true);
        let mut root=root;
        root.args=Some(budget_json::Document::parse(args,domain)?);
        Ok(root)
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_operation_prepare(
    op: u32,
    p: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    handle: *mut *mut PreparedOperation,
    out: *mut Response,
) -> i32 {
    fm_operation_prepare_budget(op,p,n,data,len,0,handle,out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_operation_prepare_budget(
    op:u32,p:*const u8,n:usize,data:*const u8,len:usize,budget_id:u64,
    handle:*mut *mut PreparedOperation,out:*mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        if !matches!(op, 1 | 2 | 4 | 5 | 8) {
            return Err("Unsupported prepared operation".into());
        }
        let domain = budget::resolve(budget_id)?;
        let root=PreparedOperation::new(op,bytes(p,n)?,bytes(data,len)?,&domain)?;
        *handle=root.into_raw_value();
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_operation_free(p: *mut PreparedOperation) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<PreparedOperation>::from_raw_value(p))));
    }
}
#[no_mangle]
pub unsafe extern "C" fn fm_writer_prepared(
    p: *mut Writer,
    operation: *const PreparedOperation,
    data: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = writer_guard(out, |out| {
        let op = operation.as_ref().ok_or("Null operation")?;
        writer_control(
            p,
            op.op,
            budget_json::Control::Charged(op.args.as_ref().unwrap().view()),
            if op.op == 1 {
                &op.data
            } else {
                bytes(data, n)?
            },
            out,
        )
    });
    writer_result(p, status)
}

pub struct Snapshot {
    pub memory_peak: u64,
    pub cache: chunk_cache::ChunkCache,
    pub stats: memory::Statistics,
    pub data: memory::Source,
    pub options: memory::Options,
    summary: Option<SharedSummary>,
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_summary(p: *const Snapshot, out: *mut Response) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        if let Some(s) = &h.summary {
            summary_response::respond(out, &h.options.domain, s)?;
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_bytes(
    data: *const u8,
    n: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    fm_snapshot_bytes_options(data, n, ptr::null(), 0, handle, out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_bytes_options(
    data: *const u8,
    n: usize,
    config: *const u8,
    config_len: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let options = if config_len == 0 {
            memory::Options::try_default()?
        } else {
            memory::Options::parse_config(bytes(config, config_len)?)?
        };
        let data = memory::Backing::copy(bytes(data, n)?, options.clone())?;
        let summary = mcap::Summary::read_with_memory_budget(&data, options.domain.clone())?;
        let domain=options.domain.clone();
        let root=mcap::charged::ChargedBox::new_fixed(Snapshot {
            memory_peak: 0,
            cache: chunk_cache::ChunkCache::new(options.domain.clone()),
            stats: memory::Statistics::default(),
            data: memory::Source::new(data, &options.domain)?,
            options,
            summary: summary.map(|s| retain_summary(s, &domain, mcap::storage::OwnerKind::Parser)).transpose()?,
        },&domain,mcap::storage::ResourceCategory::Scratch)?;
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        *handle=root.into_raw_value();
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_footer(
    data: *const u8,
    n: usize,
    dest: *mut u8,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let f = mcap::read::footer(bytes(data, n)?)?;
        let (_, b) = buffer_reader::encode(records::Record::Footer(f))?;
        copy_body(&b, dest, 20, out)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_offset(
    offset: u64,
    compression: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let compression = std::str::from_utf8(bytes(compression, n)?)?;
        out.value = mcap::shared_chunk_index::compressed_data_offset(offset, compression.len())?;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_summary_records(
    kind: u32,
    p: *const std::ffi::c_void,
    handle: *mut *mut buffer_reader::BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() || p.is_null() {
            return Err("Null handle".into());
        }
        *handle = ptr::null_mut();
        let summary = match kind {
            0 => (*(p as *const Snapshot)).summary.as_ref(),
            1 => (*(p as *const EngineHandle)).summary.as_ref(),
            2 => (*(p as *const Writer)).native_summary.as_ref(),
            _ => return Err("Unknown summary source".into()),
        };
        let options = match kind {
            0 => (*(p as *const Snapshot)).options.clone(),
            1 => (*(p as *const EngineHandle)).delivery.options.clone(),
            _ => memory::Options::with_domain((*(p as *const Writer)).domain.clone()),
        };
        let cursor = buffer_reader::summary_records(summary.ok_or("No summary available")?.clone_with_owner(mcap::storage::OwnerKind::Parser), options)?;
        *handle = cursor.into_handle()?;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_channel(
    p: *const Snapshot,
    id: u16,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let h = p.as_ref().ok_or("Null snapshot")?;
        let s = h.summary.as_ref().ok_or("No summary")?;
        buffer_reader::describe_shared_channel(
            s.channels
                .get(&id)
                .ok_or(mcap::McapError::UnknownChannel(0, id))?,
            out, &h.options.domain,
        )
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_open(
    reader: *mut Reader,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    fm_snapshot_open_options(reader, ptr::null(), 0, handle, out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_open_options(
    reader: *mut Reader,
    config: *const u8,
    config_len: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let r = reader.as_mut().ok_or("Null reader")?;
        if !r.input.seekable() {
            return Err("Snapshot requires a seekable source".into());
        }
        let options = if config_len == 0 {
            r.delivery.options.clone()
        } else {
            memory::Options::parse_config(bytes(config, config_len)?)?
        };
        let pos = r.input.stream_position()?;
        let result = (|| -> Outcome<memory::Backing> {
            r.input.seek(SeekFrom::Start(0))?;
            let length = usize::try_from(r.input.seek(SeekFrom::End(0))?)?;
            memory::check(&options.domain, "OwnedInput", options.owned, length)?;
            memory::check(&options.domain, "StorageBlock",Some(options.domain.limits().block as u64),length)?;
            let mut charge=options.domain.reserve(length)?;
            r.input.seek(SeekFrom::Start(0))?;
            let mut data = Vec::new();
            data.try_reserve_exact(length)?;
            memory::check(&options.domain, "OwnedInput", options.owned, data.capacity())?;
            charge.resize(data.capacity())?;
            data.resize(length, 0);
            r.input.read_exact(&mut data)?;
            Ok(memory::Backing::owned(data, charge))
        })();
        r.input.seek(SeekFrom::Start(pos))?;
        let data = result?;
        let summary = mcap::Summary::read_with_memory_budget(&data, options.domain.clone())?;
        let domain=options.domain.clone();
        let root=mcap::charged::ChargedBox::new_fixed(Snapshot {
            memory_peak: 0,
            cache: chunk_cache::ChunkCache::new(options.domain.clone()),
            stats: memory::Statistics::default(),
            data: memory::Source::new(data, &options.domain)?,
            options,
            summary: summary.map(|s| retain_summary(s, &domain, mcap::storage::OwnerKind::Parser)).transpose()?,
        },&domain,mcap::storage::ResourceCategory::Scratch)?;
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        *handle=root.into_raw_value();
        Ok(0)
    })
}
pub(super) unsafe fn copy_body(
    data: &[u8],
    dest: *mut u8,
    capacity: usize,
    out: &mut Response,
) -> Outcome<i32> {
    out.value = data.len() as u64;
    if capacity < data.len() {
        return Ok(2);
    }
    if !data.is_empty() {
        if dest.is_null() {
            return Err("Null destination".into());
        }
        ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
    }
    Ok(0)
}
pub(super) fn record_body(data: &[u8], offset: u64, expected: u8) -> Outcome<&[u8]> {
    let offset = usize::try_from(offset)?;
    let h = data
        .get(offset..)
        .and_then(|d| d.get(..9))
        .ok_or(mcap::McapError::BadIndex)?;
    if h[0] != expected {
        return Err(mcap::McapError::BadIndex.into());
    }
    let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
    Ok(data
        .get(offset + 9..)
        .and_then(|d| d.get(..n))
        .ok_or(mcap::McapError::BadIndex)?)
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_call(
    p: *mut Snapshot,
    op: u32,
    index_data: *const u8,
    index_length: usize,
    _message_time: u64,
    message_offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
) -> i32 {
    snapshot_call(p, op, index_data, index_length, _message_time, message_offset, dest, capacity, header, out, None)
}
unsafe fn snapshot_call(
    p: *mut Snapshot,
    op: u32,
    index_data: *const u8,
    index_length: usize,
    _message_time: u64,
    message_offset: u64,
    dest: *mut u8,
    capacity: usize,
    header: *mut MessageHeader,
    out: *mut Response,
    prepared: Option<&PreparedChunkIndex>,
) -> i32 {
    let status = guard(out, |out| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        if !header.is_null() {
            *header = MessageHeader::default();
        }
        h.stats.peak = h.stats.current;
        h.cache.stats.peak = h.cache.stats.current;
        let current = h.stats.current + h.cache.stats.current;
        h.memory_peak = h.memory_peak.max(current);
        let key = bytes(index_data, index_length)?;
        h.stats.peak = h.stats.current;
        if op == 6 {
            let f = mcap::read::footer(&h.data)?;
            let mut b = [0u8; 20];
            b[..8].copy_from_slice(&f.summary_start.to_le_bytes());
            b[8..16].copy_from_slice(&f.summary_offset_start.to_le_bytes());
            b[16..].copy_from_slice(&f.summary_crc.to_le_bytes());
            return copy_body(&b, dest, capacity, out);
        }
        match op {
            2 | 5 | 8 => {
                let parsed;
                let index = if let Some(prepared) = prepared { &prepared.index } else {
                    parsed = parse_chunk_index(bytes(index_data, index_length)?, &h.options.domain)?;
                    &parsed
                };
                if op == 8 {
                    out.value = index.compressed_data_offset()?;
                    return Ok(0);
                }
                let s = h.summary.as_ref().ok_or("File has no summary")?;
                if op == 2 {
                    check_index_range(&h.data, index.chunk_start_offset, index.chunk_length, 9)?;
                }
                if op == 2 {
                    let cached = h.cache.read(&h.data, s, &index, key, message_offset,
                        h.options.random, dest, capacity, header, out);
                    match cached {
                        Ok(Some(status)) => {
                            // Common epilogue counts caller delivery once.
                            if status == 0 { h.cache.stats.copied -= out.value; }
                            return Ok(status);
                        }
                        Ok(None) => {},
                        Err(e) => { h.cache.clear(); return Err(e); }
                    }
                    return Err("Chunk cache did not produce a result".into());
                }
                for offset in index.message_index_offsets.values() {
                    check_index_range(&h.data, *offset, 15, 15)?;
                }
                let packed=h.cache.message_indexes(&h.data,s,index,key,h.options.random)?;
                packed.copy_to(dest,capacity,out)
            }
            3 => {
                let index = mcap::shared_metadata_index::SharedMetadataIndex::read(
                    bytes(index_data, index_length)?, &h.options.domain,
                    mcap::storage::OwnerKind::Operation)?;
                let body = record_input::indexed_body(&h.data, index.offset, index.length,
                    records::op::METADATA, &h.options.domain)?;
                let status = copy_body(body.as_ref(), dest, capacity, out)?;
                if status == 0 {h.options.domain.copy_bytes(mcap::storage::CopyKind::Delivery, body.as_ref().len());}
                Ok(status)
            }
            4 => {
                let index = mcap::shared_attachment_index::SharedAttachmentIndex::read(
                    bytes(index_data, index_length)?, &h.options.domain,
                    mcap::storage::OwnerKind::Operation)?;
                let body = record_input::indexed_body(&h.data, index.offset, index.length,
                    records::op::ATTACHMENT, &h.options.domain)?;
                let status = copy_body(body.as_ref(), dest, capacity, out)?;
                if status == 0 {h.options.domain.copy_bytes(mcap::storage::CopyKind::Delivery, body.as_ref().len());}
                Ok(status)
            }
            _ => Err("Unknown snapshot operation".into()),
        }
    });
    if let Some(h) = p.as_mut() {
        if status == 0 && matches!(op, 2 | 3 | 4 | 5 | 6) && !out.is_null() {
            h.stats.copied += (*out).value;
        }
        h.memory_peak = h.memory_peak
            .max(h.stats.peak + h.cache.stats.current)
            .max(h.cache.stats.peak + h.stats.current);
    }
    status
}
// Reject overflowing/out-of-bounds caller indexes before upstream slice arithmetic.
fn check_index_range(data: &[u8], offset: u64, length: u64, minimum: u64) -> Outcome<()> {
    if length < minimum
        || offset
            .checked_add(length)
            .is_none_or(|end| end > data.len() as u64)
    {
        return Err(mcap::McapError::BadIndex.into());
    }
    Ok(())
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_free(p: *mut Snapshot) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<Snapshot>::from_raw_value(p))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_parse_record(
    op: u8,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let data=bytes(p,n)?;
        if !mcap::read::validate_borrowed_record(op,data)? {
            let domain=mcap::storage::BudgetRef::try_default()?;
            record_input::validate(op,data,&domain)?;
        }
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_reader_record_into(
    p: *mut Reader,
    offset: u64,
    dest: *mut u8,
    capacity: usize,
    opcode: *mut u8,
    out: *mut Response,
) -> i32 {
    guard(out, |out| {
        let r = p.as_mut().ok_or("Null reader")?;
        if r.failed { return Err("Reader failed".into()); }
        let pos = r.input.stream_position()?;
        let result = (|| {
            r.input.seek(SeekFrom::Start(offset))?;
            let mut h = [0u8; 9];
            r.input.read_exact(&mut h)?;
            let n = usize::try_from(u64::from_le_bytes(h[1..].try_into()?))?;
            if r.limit.is_some_and(|limit| n > limit) {
                return Err(mcap::McapError::RecordTooLarge { opcode: h[0], len: n as u64 }.into());
            }
            let start = r.input.stream_position()?;
            let end = r.input.seek(SeekFrom::End(0))?;
            if n as u64 > end.saturating_sub(start) { return Err("Record exceeds source length".into()); }
            let status = if let Input::Map { mapping, .. } = &r.input {
                let start = usize::try_from(start)?;
                let data = &mapping[start..start + n];
                record_input::validate(h[0], data, &r.delivery.options.domain)?;
                copy_body(data, dest, capacity, out)?
            } else {
                r.input.seek(SeekFrom::Start(start))?;
                r.scratch.data.clear();
                r.scratch.reserve_scratch(n)?;
                r.update_memory_peak();
                r.scratch.data.resize(n, 0);
                r.input.read_exact(&mut r.scratch.data)?;
                r.scratch.stats.copied += n as u64;
                record_input::validate(h[0], &r.scratch.data, &r.delivery.options.domain)?;
                copy_body(&r.scratch.data, dest, capacity, out)?
            };
            if status == 0 { r.scratch.stats.copied += n as u64; }
            if !opcode.is_null() { *opcode = h[0]; }
            Ok(status)
        })();
        let restored = r.input.seek(SeekFrom::Start(pos));
        r.update_memory_peak();
        r.scratch.release();
        // Budget rejection is terminal; other random-access errors retain existing semantics.
        if result.as_ref().err().is_some_and(|e: &Error| e.downcast_ref::<memory::Limit>().is_some()) {
            r.failed = true; r.parser = None; r.indexed = None;
            r.delivery.discard(); r.scratch.discard(); r.arena.clear();
        }
        restored?;
        result
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_prepare(
    p: *const u8, n: usize, data: *const u8, len: usize,
    handle: *mut *mut PreparedChannel, out: *mut Response,
) -> i32 { fm_channel_prepare_budget(p, n, data, len, 0, handle, out) }
#[no_mangle]
pub unsafe extern "C" fn fm_channel_prepare_budget(
    p: *const u8, n: usize, data: *const u8, len: usize, budget_id: u64,
    handle: *mut *mut PreparedChannel, out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let handle = handle.as_mut().ok_or("Null output")?;
        *handle = ptr::null_mut();
        let domain = budget::resolve(budget_id)?;
        *handle = PreparedChannel::new(bytes(p,n)?, bytes(data,len)?, &domain)?.into_raw_value();
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_channel_free(p: *mut PreparedChannel) {
    if !p.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<PreparedChannel>::from_raw_value(p))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_full_message(
    handle: *mut Writer,
    channel: *const PreparedChannel,
    h: *const MessageHeader,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let c = channel.as_ref().ok_or("Null channel")?;
        let h = h.as_ref().ok_or("Null header")?;
        let v = budget_json::Control::Charged(c.args.view());
        let schema = match c.schema_id {
            None => None,
            Some(id) => Some((id, v.get("schema").string("name")?, v.get("schema").string("encoding")?, c.data.as_slice())),
        };
        w.inner.as_mut().ok_or("Writer completed")?.write_borrowed(
            c.id, v.string("topic")?, v.string("encoding")?, v.get("metadata").strings()?, schema,
            &records::MessageHeader { channel_id: h.channel_id, sequence: h.sequence, log_time: h.log_time, publish_time: h.publish_time }, bytes(p,n)?,
        )?;
        Ok(0)
    });
    writer_result(handle, status)
}

#[no_mangle]
pub unsafe extern "C" fn fm_writer_private(
    handle: *mut Writer,
    opcode: u8,
    chunks: bool,
    p: *const u8,
    n: usize,
    out: *mut Response,
) -> i32 {
    let status = guard(out, |_| {
        let w = handle.as_mut().ok_or("Null writer")?;
        if w.failed || w.attachment.is_some() {
            return Err("Writer unavailable".into());
        }
        let opts = if chunks {
            enumset::enum_set!(mcap::write::PrivateRecordOptions::IncludeInChunks)
        } else {
            enumset::EnumSet::new()
        };
        w.inner
            .as_mut()
            .ok_or("Writer completed")?
            .write_private_record(opcode, bytes(p, n)?, opts)?;
        Ok(0)
    });
    writer_result(handle, status)
}

const _: () = assert!(std::mem::size_of::<Event>() == 56);
const _: () = assert!(std::mem::offset_of!(Event, header) == 32);

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_chunk_reader(
    p: *const Snapshot,
    index_data: *const u8,
    index_length: usize,
    handle: *mut *mut buffer_reader::BufferReader,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let h = p.as_ref().ok_or("Null snapshot")?;
        let index = parse_chunk_index(bytes(index_data, index_length)?, &h.options.domain)?;
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let cursor = buffer_reader::chunk_reader(h.data.clone(), summary.clone_with_owner(mcap::storage::OwnerKind::Parser), &index, h.options.clone())?;
        *handle = cursor.into_handle()?;
        Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_mapped(
    config: *const u8,
    n: usize,
    handle: *mut *mut Snapshot,
    out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() {
            return Err("Null output".into());
        }
        *handle = ptr::null_mut();
        let (document,domain)=budget_json::Document::configured(bytes(config,n)?, &["options","Budget","id"])?;
        let v=document.view();
        let options = memory::Options::parse_view(v.get("options"),domain)?;
        let data = memory::Backing::open(v.get("path").as_str().ok_or("Missing string: path")?, &options.domain)?;
        let summary = mcap::Summary::read_with_memory_budget(&data, options.domain.clone())?.map(|s| retain_summary(s, &options.domain, mcap::storage::OwnerKind::Parser)).transpose()?;
        let domain=options.domain.clone();
        let root=mcap::charged::ChargedBox::new_fixed(Snapshot {
            memory_peak: 0,
            cache: chunk_cache::ChunkCache::new(options.domain.clone()),
            stats: memory::Statistics::default(),
            data: memory::Source::new(data, &options.domain)?,
            options,
            summary,
        },&domain,mcap::storage::ResourceCategory::Scratch)?;
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        *handle=root.into_raw_value();
        Ok(0)
    })
}

// Immutable owned index: no managed dictionaries or borrowed record memory survive preparation.
pub struct PreparedChunkIndex {
    // Drop actual storage before its reservations.
    index: mcap::shared_chunk_index::SharedChunkIndex,
    key: Vec<u8>,
    _key_charge:mcap::storage::Reservation,
}
impl PreparedChunkIndex {
    pub(super) fn new(bytes: &[u8], domain: &mcap::storage::BudgetRef) -> Outcome<mcap::charged::ChargedBox<Self>> {
        let n = bytes.len();
        let index = parse_chunk_index(bytes, domain)?;
        let (mut key,key_charge)=mcap::charged::vector_fixed(domain,mcap::storage::ResourceCategory::Index,n)?;
        key.extend_from_slice(bytes);
        domain.copy_bytes(mcap::storage::CopyKind::Other,n);
        key_charge.owner_reference(mcap::storage::OwnerKind::Operation,true);
        let root=mcap::charged::ChargedBox::new_fixed(PreparedChunkIndex {
            index,key,_key_charge:key_charge,
        },domain,mcap::storage::ResourceCategory::Index)?;
        root.charge_owner(mcap::storage::OwnerKind::Operation,true);
        Ok(root)
    }
}
fn parse_chunk_index(data: &[u8], domain: &mcap::storage::BudgetRef) -> Outcome<mcap::shared_chunk_index::SharedChunkIndex> {
    Ok(mcap::shared_chunk_index::SharedChunkIndex::read(data, domain, mcap::storage::OwnerKind::Operation)?)
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_prepare(data: *const u8, n: usize, handle: *mut *mut PreparedChunkIndex, out: *mut Response) -> i32 {
    fm_chunk_index_prepare_budget(data,n,0,handle,out)
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_prepare_budget(data:*const u8,n:usize,budget_id:u64,handle:*mut *mut PreparedChunkIndex,out:*mut Response)->i32 {
    guard(out, |_| {
        if handle.is_null() { return Err("Null output".into()); }
        *handle = ptr::null_mut();
        let domain=budget::resolve(budget_id)?;
        let root = PreparedChunkIndex::new(bytes(data,n)?, &domain)?;
        *handle=root.into_raw_value();
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_chunk_index_free(p: *mut PreparedChunkIndex) {
    if !p.is_null() { let _ = catch_unwind(AssertUnwindSafe(|| drop(mcap::charged::ChargedBox::<PreparedChunkIndex>::from_raw_value(p)))); }
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_prepared_call(
    p: *mut Snapshot, op: u32, index: *const PreparedChunkIndex, time: u64, offset: u64,
    dest: *mut u8, capacity: usize, header: *mut MessageHeader, out: *mut Response,
) -> i32 {
    let Some(index) = index.as_ref() else { return guard(out, |_| Err("Null prepared index".into())); };
    snapshot_call(p, op, index.key.as_ptr(), index.key.len(), time, offset, dest, capacity, header, out, Some(index))
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_prepared_chunk_reader(
    p: *const Snapshot, index: *const PreparedChunkIndex, handle: *mut *mut buffer_reader::BufferReader, out: *mut Response,
) -> i32 {
    guard(out, |_| {
        if handle.is_null() { return Err("Null output".into()); }
        *handle = ptr::null_mut();
        let h = p.as_ref().ok_or("Null snapshot")?;
        let index = index.as_ref().ok_or("Null prepared index")?;
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let cursor = buffer_reader::chunk_reader(h.data.clone(), summary.clone_with_owner(mcap::storage::OwnerKind::Parser), &index.index, h.options.clone())?;
        *handle = cursor.into_handle()?;
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_message_owned(
    p: *mut Snapshot, data: *const u8, n: usize, prepared: *const PreparedChunkIndex,
    _time: u64, offset: u64, sink: memory::Sink, out: *mut Response,
) -> i32 {
    guard(out, |_| {
        let h = p.as_mut().ok_or("Null snapshot")?;
        let parsed;
        let (index, key) = if let Some(prepared) = prepared.as_ref() { (&prepared.index, prepared.key.as_slice()) }
        else { let key = bytes(data, n)?; parsed = parse_chunk_index(key, &h.options.domain)?; (&parsed, key) };
        check_index_range(&h.data, index.chunk_start_offset, index.chunk_length, 9)?;
        let summary = h.summary.as_ref().ok_or("File has no summary")?;
        let result = h.cache.read_with(&h.data, summary, index, key, offset, h.options.random,
            ptr::null_mut(), 0, ptr::null_mut(), &mut Response::default(), Some(sink));
        h.memory_peak = h.memory_peak.max(h.stats.current + h.cache.stats.peak);
        match result {
            Ok(Some(_)) => return Ok(0),
            Err(e) => { h.cache.clear(); return Err(e); }
            Ok(None) => {}
        }
        Err("Chunk cache did not produce a result".into())
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_wait_prepare(p:*mut EngineHandle,out:*mut Response)->i32 {
    guard(out, |_| {
        let h=p.as_mut().ok_or("Null engine")?;
        if h.failed {return Err("Engine failed".into());}
        if h.capacity_ticket.is_none() {
            h.capacity_ticket=Some(mcap::storage::CapacityWaitTicket::new(&h.delivery.options.domain)?);
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_engine_wait_status(p:*mut EngineHandle,cancel:bool,out:*mut Response)->i32 {
    guard(out, |_| {
        let h=p.as_mut().ok_or("Null engine")?;
        let ticket=h.capacity_ticket.as_mut().ok_or("Wait ticket was not prepared")?;
        if cancel {ticket.cancel();return Ok(0);}
        Ok(match ticket.checked_status()? {
            mcap::storage::CapacityWaitStatus::Idle=>0,
            mcap::storage::CapacityWaitStatus::Waiting=>1,
            mcap::storage::CapacityWaitStatus::Ready=>2,
            mcap::storage::CapacityWaitStatus::Unavailable=>unreachable!(),
        })
    })
}
fn arm_engine_wait(ticket:&mut Option<mcap::storage::CapacityWaitTicket>,domain:&mcap::storage::BudgetRef,requested:usize)->Outcome<()> {
    if let Some(ticket)=ticket {ticket.arm_for_external_release(requested)?;}
    else {domain.retry_status(requested)?;}
    Ok(())
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_input_buffer(p:*mut EngineHandle,n:usize,data:*mut *mut u8,out:*mut Response)->i32 {
    guard(out, |_| {
        let h=p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting || h.event.kind!=1 || n as u64>h.event.length {return Err("No matching input request".into());}
        let Engine::Linear(r)=&mut h.engine else {return Err("Linear engine required".into());};
        let target=data.as_mut().ok_or("Null output")?; *target=ptr::null_mut();
        // Reserve the parser's complete request before a short asynchronous read.
        // Reserving only the managed read quantum would repeatedly relocate large records.
        match r.try_insert(usize::try_from(h.event.length)?) {
            Ok(b)=>*target=b.as_mut_ptr(),
            Err(e)=>{
                let e:Error=e.into();
                if budget::unavailable(&e) {
                    let requested=budget::requested_capacity(&e).ok_or("Missing retry capacity")?;
                    arm_engine_wait(&mut h.capacity_ticket,&h.delivery.options.domain,requested)?;
                    return Ok(4);
                }
                return Err(e);
            }
        }
        Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_engine_input_complete(p:*mut EngineHandle,n:usize,out:*mut Response)->i32 {
    guard(out, |_| {
        let h=p.as_mut().ok_or("Null engine")?;
        if h.failed || !h.waiting || h.event.kind!=1 || n as u64>h.event.length {return Err("No matching input request".into());}
        let Engine::Linear(r)=&mut h.engine else {return Err("Linear engine required".into());};
        r.notify_read(n); h.waiting=false; Ok(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn fm_engine_lease_step(p:*mut EngineHandle,count:usize,target:usize,
    output:*mut *mut lease::Handle,event:*mut Event,out:*mut Response)->i32 {
    if !output.is_null() { *output=ptr::null_mut(); }
    let status=guard(out, |out| {
        let h=p.as_mut().ok_or("Null engine")?;
        if h.failed || output.is_null() || event.is_null() || count==0 || count>65536 || target==0 { return Err("Invalid lease step".into()); }
        *event=Event::default();
        if h.ended { return Ok(1); }
        if h.waiting {
            if h.event.kind!=1 { return Err("Consume the pending record before switching to leases".into()); }
            *event=h.event; return Ok(0);
        }
        let Engine::Linear(r)=&mut h.engine else {return Err("Linear engine required".into());};
        r.set_memory_budget(h.delivery.options.domain.clone())?;
        if h.lease_batch.is_none() {
            match lease::Batch::new(h.delivery.options.domain.clone(),count) {
                Ok(b)=>h.lease_batch=Some(b),
                Err(ref e) if budget::unavailable(e)=>{
                    arm_engine_wait(&mut h.capacity_ticket,&h.delivery.options.domain,lease::Batch::reservation_bytes(count)?)?;
                    return Ok(4);
                },
                Err(e)=>return Err(e),
            }
        }
        let mut used=0usize;
        loop {
            let next=match r.next_shared_event().transpose() {
                Ok(e)=>e,
                Err(e)=>{
                    let e:Error=e.into();
                    if budget::unavailable(&e) {
                        if h.lease_batch.as_ref().unwrap().messages.is_empty() {
                            let batch=h.lease_batch.take().unwrap();
                            let rollback=batch.allocation_bytes().checked_add(batch.messages.allocated_bytes()).ok_or("Retry capacity overflow")?;
                            let requested=budget::requested_capacity(&e).and_then(|n|n.checked_add(rollback)).ok_or("Missing retry capacity")?;
                            drop(batch);
                            arm_engine_wait(&mut h.capacity_ticket,&h.delivery.options.domain,requested)?;
                            return Ok(4);
                        }
                        break;
                    }
                    return Err(e);
                }
            };
            match next {
                None=>{h.ended=true;break;}
                Some(sans_io::linear_reader::SharedReadEvent::ReadRequest(n))=>{
                    h.event=Event {kind:1,length:n as u64,..Default::default()}; h.waiting=true;
                    if h.lease_batch.as_ref().unwrap().messages.is_empty() { *event=h.event; return Ok(0); }
                    break;
                }
                Some(sans_io::linear_reader::SharedReadEvent::Record {opcode,data})=>{
                    if opcode != records::op::MESSAGE {
                        buffer_reader::BufferReader::observe(&mut h.schemas, &mut h.channels, opcode, data.as_ref(), &h.delivery)?;
                        continue;
                    }
                    match mcap::parse_record(opcode,data.as_ref())? {
                        records::Record::Message {header,..}=>{
                            if !h.channels.contains_key(&header.channel_id) {return Err(mcap::McapError::UnknownChannel(header.sequence,header.channel_id).into());}
                            let header=buffer_reader::native_header(&header);
                            let payload=data.slice(22..data.as_ref().len()); used+=payload.as_ref().len();
                            let batch=h.lease_batch.as_mut().unwrap();
                            batch.messages.push(lease::Message {header,data:payload})?;
                            // A batch retained across an input request keeps its original preallocated limit.
                            if batch.messages.len()>=count.min(batch.message_limit) || used>=target {break;}
                        }
                        _=>unreachable!(),
                    }
                }
            }
        }
        let batch=h.lease_batch.take().unwrap();
        out.value=batch.messages.len() as u64;
        if !batch.messages.is_empty() { *output=lease::publish(batch); return Ok(0); }
        Ok(if h.ended {1} else {4})
    });
    if status<0 { if let Some(h)=p.as_mut() { h.failed=true; h.lease_batch=None; h.engine=Engine::Inactive; } }
    status
}

#[repr(C)]
pub struct SeekRequest { index:*const PreparedChunkIndex, time:u64, offset:u64 }
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_seek_batch(p:*mut Snapshot,requests:*const SeekRequest,count:usize,
    output:*mut *mut lease::Handle,out:*mut Response)->i32 {
    guard(out, |_| {
        let result=output.as_mut().ok_or("Null output")?; *result=ptr::null_mut();
        let h=p.as_mut().ok_or("Null snapshot")?;
        if count==0 || count>65536 || requests.is_null() {return Err("Invalid seek batch".into());}
        let requests=slice::from_raw_parts(requests,count);
        let summary=h.summary.as_ref().ok_or("File has no summary")?;
        // Validate all descriptor handles before loading anything.
        for req in requests { if req.index.is_null() {return Err("Null prepared index".into());} }
        let mut order=mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
            h.options.domain.clone(),mcap::storage::ResourceCategory::Scratch,count);
        order.reserve(count)?;
        order.owner_reference(mcap::storage::OwnerKind::Operation,true);
        for i in 0..count { order.push(i)?; }
        order.sort_by_key(|i|((*requests[*i].index).key.as_slice(),*i));
        let mut batch=lease::Batch::new(h.options.domain.clone(),count)?;
        let mut previous:Option<&[u8]>=None;
        let mut chunk=None;
        // Output slots are filled in request order after sorting compact descriptors.
        let mut slots=mcap::segmented::BudgetedSegmentedVec::with_page_capacity(
            h.options.domain.clone(),mcap::storage::ResourceCategory::Descriptor,count);
        slots.reserve(count)?;
        slots.owner_reference(mcap::storage::OwnerKind::Operation,true);
        for _ in 0..count { slots.push(None)?; }
        for &i in &order {
            let req=&requests[i];let index=&*req.index;
            if previous!=Some(index.key.as_slice()) {
                chunk=Some(h.cache.load(&h.data,&index.index,&index.key,h.options.random)?);
                previous=Some(&index.key);
            }
            slots[i]=Some(chunk.as_ref().unwrap().message(summary,req.offset)?);
        }
        for i in 0..count {batch.messages.push(slots[i].take().unwrap())?;}
        *result=lease::publish(batch);Ok(0)
    })
}
#[no_mangle]
pub unsafe extern "C" fn fm_snapshot_cache_statistics(p:*const Snapshot,hits:*mut u64,loads:*mut u64,out:*mut Response)->i32 {
    guard(out, |_| {let h=p.as_ref().ok_or("Null snapshot")?;*hits.as_mut().ok_or("Null hits")?=h.cache.hits;*loads.as_mut().ok_or("Null loads")?=h.cache.loads;Ok(0)})
}

#[cfg(test)]
mod root_tests {
    use super::*;
    #[test]
    fn summary_publication_refusal_after_parser_eof_is_terminal() {
        let mut writer = mcap::WriteOptions::new().use_chunks(false)
            .create(std::io::Cursor::new(Vec::new())).unwrap();
        writer.finish().unwrap();
        let data = writer.into_inner().into_inner();
        let domain = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {retained:0,..Default::default()}).unwrap();
        let mut parser = sans_io::SummaryReader::new_with_options_and_budget(
            sans_io::SummaryReaderOptions::default().with_file_size(data.len() as u64), domain.clone());
        let mut input = std::io::Cursor::new(&data);
        while let Some(event) = parser.next_event() {
            match event.unwrap() {
                sans_io::SummaryReadEvent::ReadRequest(n) => {
                    let count = input.read(parser.try_insert(n).unwrap()).unwrap();
                    parser.notify_read(count);
                }
                sans_io::SummaryReadEvent::SeekRequest(to) => {
                    let position = input.seek(to).unwrap();
                    parser.notify_seeked(position);
                }
            }
        }
        let mut engine = EngineHandle {
            capacity_ticket:None,lease_batch:None,
            schemas:buffer_reader::SchemaTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            channels:buffer_reader::ChannelTable::new_owned(domain.clone(),mcap::storage::ResourceCategory::Declaration,mcap::storage::OwnerKind::Parser),
            engine:Engine::Summary(Some(parser)), event:Event::default(),
            delivery:memory::Delivery::new(memory::Options::with_domain(domain.clone())),
            waiting:false,failed:false,ended:false,summary:None,
        };
        domain.fail_allocation_at(0);
        unsafe {
            assert_eq!(fm_engine_next(&mut engine,ptr::null_mut(),0,&mut Event::default(),&mut Response::default()),-1);
            assert!(engine.failed);
            assert!(engine.summary.is_none());
            assert!(matches!(engine.engine,Engine::Inactive));
            assert_eq!(fm_engine_next(&mut engine,ptr::null_mut(),0,&mut Event::default(),&mut Response::default()),-1);
        }
        drop(engine);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
    #[test]
    fn prepared_operation_parse_failure_releases_root_payload_and_pins() {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        assert!(PreparedOperation::new(1,b"{",&[42;4096],&domain).is_err());
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        assert_eq!(domain.workload_detailed_statistics().allocation_count,3);
    }
    #[test]
    fn snapshot_root_remains_charged_until_matching_free() {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let root=mcap::charged::ChargedBox::new_fixed(Snapshot {
            memory_peak:0,cache:chunk_cache::ChunkCache::new(domain.clone()),
            stats:Default::default(),data:memory::Source::empty(),
            options:memory::Options { domain:domain.clone(), ..Default::default() },summary:None,
        },&domain,mcap::storage::ResourceCategory::Scratch).unwrap();
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        assert_eq!(domain.workload_statistics().current,root.allocation_bytes() as u64);
        unsafe { fm_snapshot_free(root.into_raw_value()); }
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
}

#[cfg(test)]
mod prepared_channel_tests {
    use super::*;
    #[test]
    fn every_prepared_channel_allocation_refusal_rolls_back() {
        let input = br#"{"id":1,"topic":"topic","encoding":"raw","metadata":{"a":"one","z":"two"},"schema":{"id":2,"name":"schema","encoding":"raw"}}"#;
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let root = PreparedChannel::new(input, &[42;4096], &domain).unwrap();
        let allocations = domain.workload_detailed_statistics().allocation_count;
        drop(root);
        assert_eq!(domain.workload_statistics().current, 0);
        for index in 0..allocations {
            let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
            domain.fail_allocation_at(index as usize);
            assert!(PreparedChannel::new(input, &[42;4096], &domain).is_err());
            assert_eq!(domain.workload_statistics().current, 0);
            assert_eq!(domain.ownership_statistics(), Default::default());
        }
        let invalid = br#"{"id":1,"topic":"topic","encoding":"raw","metadata":{"a":1},"schema":null}"#;
        assert!(PreparedChannel::new(invalid, &[], &domain).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
