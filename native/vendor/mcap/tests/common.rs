use std::fs;

use anyhow::{Context, Result};
use camino::Utf8Path;
use memmap2::Mmap;

pub fn map_mcap<P: AsRef<Utf8Path>>(p: P) -> Result<Mmap> {
    let p = p.as_ref();
    let resolved = match (std::env::var_os("MCAP_CONFORMANCE_ROOT"), p.as_str().strip_prefix("../../tests/conformance/")) {
        (Some(root), Some(relative)) => std::path::PathBuf::from(root).join(relative),
        _ => p.as_std_path().to_owned(),
    };
    let fd = fs::File::open(&resolved).with_context(|| format!("Couldn't open {}", resolved.display()))?;
    unsafe { Mmap::map(&fd) }.with_context(|| format!("Couldn't map {}", resolved.display()))
}

#[allow(dead_code)]
pub fn mcap_test_file() -> Result<Mmap> {
    if cfg!(feature = "zstd") {
        map_mcap("tests/data/compressed.mcap")
    } else {
        map_mcap("tests/data/uncompressed.mcap")
    }
}

/// Materialize only the test expectation through the official owned serializer.
#[allow(dead_code)]
pub fn shared_statistics(value: mcap::records::Statistics) -> Result<mcap::shared_statistics::SharedStatistics> {
    use binrw::BinWrite;
    let mut wire = std::io::Cursor::new(Vec::new());
    value.write_le(&mut wire)?;
    wire.set_position(0);
    Ok(mcap::shared_statistics::SharedStatistics::read(
        &mut wire, Default::default(), mcap::storage::OwnerKind::Parser,
    )?)
}

#[allow(dead_code)]
pub fn shared_attachment_index(value: mcap::records::AttachmentIndex) -> Result<mcap::shared_attachment_index::SharedAttachmentIndex> {
    use binrw::BinWrite;
    let mut wire = std::io::Cursor::new(Vec::new());
    value.write_le(&mut wire)?;
    Ok(mcap::shared_attachment_index::SharedAttachmentIndex::read(
        wire.get_ref(), &Default::default(), mcap::storage::OwnerKind::Parser,
    )?)
}

#[allow(dead_code)]
pub fn shared_schema(value: &mcap::Schema<'_>) -> Result<mcap::shared_declarations::SharedSchema> {
    Ok(mcap::shared_declarations::SharedSchema::new(value.id,&value.name,&value.encoding,&value.data,&Default::default(),mcap::storage::OwnerKind::Parser)?)
}
#[allow(dead_code)]
pub fn shared_channel(value: &mcap::Channel<'_>) -> Result<mcap::shared_declarations::SharedChannel> {
    let schema = value.schema.as_ref().map(|schema| shared_schema(schema)).transpose()?;
    Ok(mcap::shared_declarations::SharedChannel::new(value.id,&value.topic,&value.message_encoding,schema,value.metadata.iter().map(|(k,v)|(k.as_str(),v.as_str())),&Default::default(),mcap::storage::OwnerKind::Parser)?)
}
