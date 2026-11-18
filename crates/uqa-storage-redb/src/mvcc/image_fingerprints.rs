//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-size revision metadata permits cut-qualified selection without fetching redb value pages.

use redb::TableDefinition;
use uqa_storage::{
    backup::{ImageDigest, UnitFingerprint},
    mvcc::{VersionError, VersionResult},
};

use crate::error::redb_error;

pub(super) const LEGACY_THROUGH: &str = "image_legacy_through";
pub(super) const TABLE: TableDefinition<(&[u8], u64), &[u8; 40]> =
    TableDefinition::new("uqa_mvcc_record_images_v1");

pub(super) fn initialize_boundary(
    metadata: &mut redb::Table<'_, &'static str, &'static [u8]>,
    format: u64,
) -> VersionResult<()> {
    let sequence = super::codec::read_u64(metadata, "sequence")?;
    if format < 65 {
        metadata
            .insert(LEGACY_THROUGH, sequence.to_be_bytes().as_slice())
            .map_err(redb_error)?;
    } else if super::codec::read_u64(metadata, LEGACY_THROUGH)? > sequence {
        return Err(VersionError::InvalidEncoding(
            "record image history starts beyond committed visibility",
        ));
    }
    Ok(())
}

pub(super) fn insert(
    table: &mut redb::Table<'_, (&'static [u8], u64), &'static [u8; 40]>,
    key: &[u8],
    revision: u64,
    live: Option<(usize, ImageDigest)>,
) -> VersionResult<()> {
    let mut record = [0_u8; 40];
    if let Some((length, digest)) = live {
        let encoded = u64::try_from(length)
            .ok()
            .and_then(|n| n.checked_add(8))
            .ok_or(VersionError::InvalidEncoding(
                "record image length overflow",
            ))?;
        record[..8].copy_from_slice(&encoded.to_be_bytes());
        record[8..].copy_from_slice(&digest.as_bytes());
    }
    table.insert((key, revision), &record).map_err(redb_error)?;
    Ok(())
}

pub(super) fn decode(record: &[u8; 40]) -> VersionResult<Option<UnitFingerprint>> {
    let decoded_bytes = u64::from_be_bytes(record[..8].try_into().unwrap());
    if decoded_bytes == 0 && record[8..] == [0; 32] {
        return Ok(None);
    }
    if decoded_bytes < 8 {
        return Err(VersionError::InvalidEncoding(
            "invalid revision image metadata",
        ));
    }
    Ok(Some(UnitFingerprint {
        decoded_bytes,
        digest: ImageDigest::from_bytes(record[8..].try_into().unwrap()),
    }))
}
