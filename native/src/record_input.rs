//! Temporary random-record bodies remain charged until response publication.
use super::*;
use mcap::storage::{OwnerKind, Reservation, ResourceCategory, SharedBytes};

pub(super) enum Body {
    Mapped(SharedBytes),
    // Field order releases the physical allocation before its reservation.
    Owned { bytes: Vec<u8>, _charge: Reservation },
}
impl AsRef<[u8]> for Body {
    fn as_ref(&self) -> &[u8] {
        match self { Self::Mapped(bytes) => bytes.as_ref(), Self::Owned {bytes,..} => bytes }
    }
}
impl Body {
    pub fn read(input: &mut Input, start: u64, length: usize, options: &memory::Options) -> Outcome<Self> {
        memory::check(&options.domain,"StorageBlock",Some(options.domain.limits().block as u64),length)?;
        if let Input::Map {mapping,..} = input {
            let start=usize::try_from(start)?;
            let end=start.checked_add(length).ok_or("Record range overflow")?;
            if end>mapping.len() {return Err("Record exceeds source length".into());}
            return Ok(Self::Mapped(mapping.shared(start..end)));
        }
        memory::check(&options.domain,"ScratchBuffer",options.scratch,length)?;
        let (mut bytes,charge)=mcap::charged::vector_fixed::<u8>(&options.domain,ResourceCategory::Input,length)?;
        charge.owner_reference(OwnerKind::Operation,true);
        bytes.resize(length,0);
        // Read into the final temporary storage; no managed staging copy.
        let mut body=Self::Owned {bytes,_charge:charge};
        if let Self::Owned {bytes,..}=&mut body {input.read_exact(bytes)?;}
        Ok(body)
    }
}

/// Validate exactly one indexed record using the official framing state machine.
/// The body remains a slice of the input owner; no owned metadata/attachment is built.
pub(super) fn indexed_body(input: &memory::Source, offset: u64, length: u64,
    expected: u8, domain: &mcap::storage::BudgetRef) -> Outcome<SharedBytes> {
    let end = offset.checked_add(length).filter(|end| *end <= input.len() as u64)
        .ok_or(mcap::McapError::BadIndex)?;
    let start = usize::try_from(offset)?;
    let end = usize::try_from(end)?;
    let mut parser = sans_io::LinearReader::new_with_options_and_budget(
        sans_io::LinearReaderOptions::default().with_record_length_limit(end - start)
            .with_skip_start_magic(true).with_skip_end_magic(true), domain.clone());
    let mut supplied = false;
    let mut body = None;
    loop {
        let event = parser.next_shared_event();
        match event {
            Some(Ok(sans_io::linear_reader::SharedReadEvent::ReadRequest(_))) => {
                if supplied { parser.notify_read(0); }
                else { parser.supply_shared(input.shared(start..end)); supplied = true; }
            }
            Some(Ok(sans_io::linear_reader::SharedReadEvent::Record {opcode, data})) => {
                if body.is_some() || opcode != expected {return Err(mcap::McapError::BadIndex.into());}
                validate(opcode, data.as_ref(), domain)?;
                body = Some(data);
            }
            Some(Err(error)) => {
                // Upstream indexed helpers reject any second event, including malformed tails.
                return Err(if body.is_some() {mcap::McapError::BadIndex.into()} else {error.into()});
            }
            None => return body.ok_or_else(|| mcap::McapError::BadIndex.into()),
        }
    }
}

/// Borrow record fields, charging only paged string-map duplicate descriptors.
/// Standalone records do not resolve schema IDs against declarations.
pub(super) fn validate(op: u8, data: &[u8], domain: &mcap::storage::BudgetRef) -> Outcome<()> {
    if mcap::read::validate_borrowed_record(op,data)? {return Ok(());}
    match op {
        records::op::CHANNEL => mcap::read::validate_channel_record(data,domain)?,
        records::op::METADATA => mcap::read::validate_metadata_record(data,domain)?,
        _ => unreachable!("all other record bodies are validated without heap storage"),
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixtures() -> Vec<(u8,Vec<u8>)> {
    use binrw::BinWrite;
    let mut result=Vec::new();
    macro_rules! fixture { ($opcode:expr,$value:expr) => {{
        let mut wire=std::io::Cursor::new(Vec::new());
        $value.write_le(&mut wire).unwrap();result.push(($opcode,wire.into_inner()));
    }}; }
    fixture!(records::op::METADATA,records::Metadata {name:"name".into(),metadata:[("key".into(),"value".into()),("second".into(),"value2".into())].into()});
    fixture!(records::op::CHANNEL,records::Channel {id:7,schema_id:513,topic:"topic\n".into(),message_encoding:"raw".into(),metadata:[("key".into(),"value".into())].into()});
    fixture!(records::op::CHUNK_INDEX,records::ChunkIndex {message_start_time:1,message_end_time:7,chunk_start_offset:19,chunk_length:200,message_index_offsets:[(0,10),(9,18),(256,20),(513,25),(768,30),(65535,40)].into(),message_index_length:50,compression:"zstd".into(),compressed_size:90,uncompressed_size:180});
    fixture!(records::op::ATTACHMENT_INDEX,records::AttachmentIndex {offset:4,length:70,log_time:9,create_time:8,data_size:4,name:"name\"".into(),media_type:"raw".into()});
    fixture!(records::op::METADATA_INDEX,records::MetadataIndex {offset:80,length:40,name:"metadata\u{4e2d}".into()});
    fixture!(records::op::STATISTICS,records::Statistics {message_count:12,channel_count:2,channel_message_counts:[(1,5),(257,7)].into(),..Default::default()});
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charged_record_validation_preserves_official_acceptance() {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        for (opcode,wire) in fixtures().into_iter().chain(borrowed_fixtures()) {
            for end in 0..=wire.len() {
                assert_eq!(validate(opcode,&wire[..end],&domain).is_ok(),mcap::parse_record(opcode,&wire[..end]).is_ok(),"opcode={opcode} end={end}");
                assert_eq!(domain.workload_statistics().current,0);
            }
            for position in 0..wire.len() {
                let mut changed=wire.clone();changed[position]^=1;
                assert_eq!(validate(opcode,&changed,&domain).is_ok(),mcap::parse_record(opcode,&changed).is_ok(),"opcode={opcode} mutation={position}");
                assert_eq!(domain.workload_statistics().current,0);
            }
            let mut trailing=wire.clone();trailing.extend_from_slice(&[7;9]);
            assert!(validate(opcode,&trailing,&domain).is_ok());
            assert!(mcap::parse_record(opcode,&trailing).is_ok());
            assert_eq!(domain.workload_statistics().current,0);
        }
    }
}

#[cfg(test)]
pub(super) fn borrowed_fixtures() -> Vec<(u8,Vec<u8>)> {
    use binrw::BinWrite;
    let mut result=Vec::new();
    macro_rules! fixture { ($opcode:expr,$value:expr) => {{
        let mut wire=std::io::Cursor::new(Vec::new());
        $value.write_le(&mut wire).unwrap();result.push(($opcode,wire.into_inner()));
    }}; }
    fixture!(records::op::HEADER,records::Header {profile:"profile\u{4e2d}".into(),library:"library".into()});
    fixture!(records::op::FOOTER,records::Footer {summary_start:10,summary_offset_start:20,summary_crc:123});
    fixture!(records::op::MESSAGE,records::MessageHeader {channel_id:1,sequence:7,log_time:9,publish_time:10});
    fixture!(records::op::SUMMARY_OFFSET,records::SummaryOffset {group_opcode:3,group_start:17,group_length:90});
    fixture!(records::op::DATA_END,records::DataEnd {data_section_crc:0});
    fixture!(records::op::MESSAGE_INDEX,records::MessageIndex {channel_id:7,records:vec![records::MessageIndexEntry {log_time:9,offset:17};3]});
    fixture!(records::op::SCHEMA,records::SchemaHeader {id:7,name:"schema\u{4e2d}".into(),encoding:"raw".into()});
    result.last_mut().unwrap().1.extend_from_slice(&4u32.to_le_bytes());
    result.last_mut().unwrap().1.extend_from_slice(&[1,2,3,4]);
    fixture!(records::op::CHUNK,records::ChunkHeader {message_start_time:1,message_end_time:2,uncompressed_size:4,uncompressed_crc:0,compression:"none".into(),compressed_size:4});
    result.last_mut().unwrap().1.extend_from_slice(&[1,2,3,4]);
    fixture!(records::op::ATTACHMENT,records::AttachmentHeader {log_time:17,create_time:23,name:"name\u{4e2d}".into(),media_type:"application/octet-stream".into()});
    let attachment=&mut result.last_mut().unwrap().1;
    attachment.extend_from_slice(&4u64.to_le_bytes());attachment.extend_from_slice(&[1,2,3,4]);
    attachment.extend_from_slice(&crc32fast::hash(attachment).to_le_bytes());
    result.push((0x90,vec![1,2,3]));
    result
}

#[cfg(test)]
mod map_page_tests {
    use super::*;
    #[test]
    fn borrowed_metadata_keys_cross_pages_without_copying_strings() {
        for duplicate in [false,true] {
            let mut wire=Vec::from(0u32.to_le_bytes());
            wire.extend_from_slice(&(10000u32*18).to_le_bytes());
            for i in (0..10000).rev() {
                let key=format!("key-{:05}",if duplicate && i==0 {1} else {i});
                wire.extend_from_slice(&9u32.to_le_bytes());wire.extend_from_slice(key.as_bytes());
                wire.extend_from_slice(&1u32.to_le_bytes());wire.push(b'x');
            }
            let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
            assert_eq!(validate(records::op::METADATA,&wire,&domain).is_ok(),!duplicate);
            assert_eq!(mcap::parse_record(records::op::METADATA,&wire).is_ok(),!duplicate);
            assert_eq!(domain.workload_statistics().current,0);
            assert_eq!(domain.ownership_statistics(),Default::default());
            assert_eq!(domain.detailed_statistics().flow.other_copy,0);
            assert!(domain.workload_detailed_statistics().allocated_bytes>65536);
        }
    }
}

#[cfg(test)]
mod indexed_tests {
    use super::*;
    fn compare(wire: &[u8], opcode: u8, length: u64) {
        let options = memory::Options::default();
        let source = memory::Source::new(memory::Backing::copy(wire, options.clone()).unwrap(), &options.domain).unwrap();
        let actual = indexed_body(&source, 0, length, opcode, &options.domain);
        let expected = if opcode == records::op::METADATA {
            mcap::read::metadata(wire, &records::MetadataIndex {offset:0,length,name:String::new()}).map(|_| ())
        } else {
            mcap::read::attachment(wire, &records::AttachmentIndex {offset:0,length,name:String::new(),media_type:String::new(),log_time:0,create_time:0,data_size:0}).map(|_| ())
        };
        assert_eq!(actual.is_ok(), expected.is_ok(), "opcode={opcode} length={length} wire={wire:?}");
        if let Ok(body) = actual {
            assert_eq!(body.as_ref().as_ptr(), unsafe {source.as_ptr().add(9)});
            let expected = body.as_ref().to_vec();
            drop(source);
            assert_eq!(body.as_ref(), expected);
        }
    }
    #[test]
    fn indexed_bodies_match_official_bounds_crc_and_second_record_rules() {
        for (opcode, body) in fixtures().into_iter().chain(borrowed_fixtures())
            .filter(|(op,_)| matches!(*op, records::op::METADATA|records::op::ATTACHMENT)) {
            let mut wire = vec![opcode];
            wire.extend_from_slice(&(body.len() as u64).to_le_bytes());
            wire.extend_from_slice(&body);
            for length in 0..=wire.len()+1 {compare(&wire, opcode, length as u64);}
            for position in 0..wire.len() {
                let mut changed=wire.clone();changed[position]^=1;
                compare(&changed,opcode,changed.len() as u64);
            }
            for tail in [vec![0],vec![0x90,0,0,0,0,0,0,0,0],wire.clone()] {
                let mut changed=wire.clone();changed.extend_from_slice(&tail);
                compare(&changed,opcode,changed.len() as u64);
            }
        }
    }
}
