mod common;

use common::*;

use std::io::BufWriter;

use anyhow::Result;
use memmap2::Mmap;
use tempfile::tempfile;

const DEFAULT_LIBRARY_LENGTH: u64 = mcap::LIBRARY_IDENTIFIER.len() as u64;

#[test]
fn smoke() -> Result<()> {
    let mapped = map_mcap("../../tests/conformance/data/OneMetadata/OneMetadata.mcap")?;
    let metas = mcap::read::LinearReader::new(&mapped)?
        .filter_map(|record| match record.unwrap() {
            mcap::records::Record::Metadata(m) => Some(m),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(metas.len(), 1);

    let expected = mcap::records::Metadata {
        name: String::from("myMetadata"),
        metadata: [(String::from("foo"), String::from("bar"))].into(),
    };

    assert_eq!(metas[0], expected);

    Ok(())
}

#[test]
fn round_trip() -> Result<()> {
    let mapped = map_mcap("../../tests/conformance/data/OneMetadata/OneMetadata.mcap")?;
    let metas =
        mcap::read::LinearReader::new(&mapped)?.filter_map(|record| match record.unwrap() {
            mcap::records::Record::Metadata(m) => Some(m),
            _ => None,
        });

    let mut tmp = tempfile()?;
    let mut writer = mcap::Writer::new(BufWriter::new(&mut tmp))?;

    for m in metas {
        writer.write_metadata(&m)?;
    }
    drop(writer);

    let ours = unsafe { Mmap::map(&tmp) }?;
    let summary = mcap::Summary::read(&ours)?;

    let mut expected_summary = mcap::Summary::default();
    expected_summary.stats = Some(shared_statistics(mcap::records::Statistics {
            metadata_count: 1,
            ..Default::default()
        })?);
    expected_summary.metadata_indexes.push(mcap::shared_metadata_index::SharedMetadataIndex::new(
        25 + DEFAULT_LIBRARY_LENGTH, 41, "myMetadata", &Default::default(), mcap::storage::OwnerKind::Parser,
    )?)?;
    assert_eq!(summary, Some(expected_summary));

    let expected = mcap::records::Metadata {
        name: String::from("myMetadata"),
        metadata: [(String::from("foo"), String::from("bar"))].into(),
    };

    assert_eq!(
        mcap::read::metadata(&ours, &summary.unwrap().metadata_indexes[0])?,
        expected
    );

    Ok(())
}
