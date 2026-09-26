//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic head and complete coverage installation on the caller's evaluated record transaction.

use crate::diskann_index::{
    build::DiskANNCanonicalCoverage,
    catalog::DiskANNIndexScope,
    format::{DiskANNGeneration, DiskANNManifest},
    pages::{DiskANNPageSource, DiskANNRecordKey},
    DiskANNCanonicalRead,
};
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueBatch, StorageBackendResult};

use super::{
    identity::require_mapping,
    invalid,
    keys::{Keys, Kind},
    staging::load_state,
    state::{fixed, State},
    DiskANNStageStatus, KeyValueDiskANNSource,
};

pub(crate) const HEAD_PREFIX: &[u8] = b"\0uqa-diskann-v1\0\x05";
const HEAD_BYTES: usize = 41;

/// The provider supplies the build's original retained view and one current mutation's read/batch. Native adapters map all three through the same physical record family. The provider must guard actual catalog records and captured private input before calling publication.
pub struct DiskANNPublicationViews<'a> {
    pub captured: &'a dyn KeyValueRead,
    pub current: &'a dyn KeyValueRead,
    pub batch: &'a mut dyn KeyValueBatch,
    pub sealed: &'a KeyValueDiskANNSource,
}

/// Provider integration boundary; ordinary callers use their concrete canonical source's publication method. Installs the actually sealed manifest's complete coverage by selecting its immutable generation, without deleting journal keys or completing a transaction. A failed call invalidates the enclosing mutation batch. Later private DDL must cancel or supersede this effect through the catalog lifecycle owner.
pub fn publish_captured_generation<S: DiskANNCanonicalRead>(
    coverage: &DiskANNCanonicalCoverage<S>,
    scope: &DiskANNIndexScope,
    parameters: DiskANNIndexParams,
    mut views: DiskANNPublicationViews<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    coverage.check_control(control)?;
    views.captured.control().check()?;
    views.current.control().check()?;
    let manifest = coverage.manifest();
    if manifest.origins() != Some(coverage.origins()) || manifest.input().parameters != parameters {
        return Err(invalid(
            "publication requires complete captured origins and catalog parameters",
        ));
    }
    let sealed = views.sealed;
    if sealed.generation() != manifest.input().generation {
        return Err(invalid("sealed source belongs to another generation"));
    }
    sealed
        .store
        .with_read_view(&mut |physical| install(coverage, scope, &mut views, physical, control))
}

fn install<S: DiskANNCanonicalRead>(
    coverage: &DiskANNCanonicalCoverage<S>,
    scope: &DiskANNIndexScope,
    views: &mut DiskANNPublicationViews<'_>,
    physical: &dyn KeyValueRead,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let manifest = coverage.manifest();
    let generation = manifest.input().generation;
    require_mapping(scope, generation, physical, views.batch, control)?;
    let key = head_key(scope);
    // Never refresh this expectation from the publishing command: a newer head may already have retired covered changes.
    let expected = read_head(views.captured, &key, control)?;
    if read_head(views.current, &key, control)? != expected {
        return Err(invalid("selected generation changed since build capture"));
    }
    let state = load_state(physical, generation, control)?
        .filter(|state| state.status == DiskANNStageStatus::Sealed)
        .ok_or_else(|| invalid("publication requires an actually sealed generation"))?;
    let keys = Keys::new(generation);
    let manifest_key = keys.key(Kind::Record(DiskANNRecordKey::Manifest));
    let bytes = fixed::<{ DiskANNManifest::MAX_ENCODED_BYTES }>(control, |visit| {
        physical.visit_value_bounded(
            manifest_key.as_ref(),
            DiskANNManifest::MAX_ENCODED_BYTES,
            control,
            visit,
        )
    })?
    .ok_or_else(|| invalid("sealed manifest is missing"))?;
    if DiskANNManifest::decode(generation, &bytes, control)? != *manifest {
        return Err(invalid(
            "sealed manifest differs from the completed build capture",
        ));
    }
    if let Some(old) = expected {
        if old == generation
            || old.database() != generation.database()
            || old.table() != generation.table()
            || old.index() != generation.index()
        {
            return Err(invalid(
                "previous head has a different physical incarnation",
            ));
        }
        let previous = load_state(views.current, old, control)?
            .filter(|state| state.status == DiskANNStageStatus::Published)
            .ok_or_else(|| invalid("previous head has no published generation"))?;
        transition(views.batch, old, previous, DiskANNStageStatus::Retired)?;
    }
    views.batch.require_unchanged(&key)?;
    require_observed(physical, manifest_key.as_ref(), views.batch)?;
    let state_key = keys.key(Kind::State);
    let revision = physical
        .record_revision(state_key.as_ref())?
        .ok_or_else(|| invalid("sealed state has no committed revision"))?;
    views.batch.put_observed(
        state_key.as_ref(),
        &State {
            status: DiskANNStageStatus::Published,
            ..state
        }
        .encode(),
        &revision,
    )?;
    views.batch.put(&key, &encode_head(generation))?;
    views.captured.control().check()?;
    views.current.control().check()?;
    coverage.check_control(control)
}

pub(super) fn require_observed(
    read: &dyn KeyValueRead,
    key: &[u8],
    batch: &mut dyn KeyValueBatch,
) -> StorageBackendResult<()> {
    let revision = read
        .record_revision(key)?
        .ok_or_else(|| invalid("physical metadata has no committed revision"))?;
    batch.require_observed(key, &revision)
}

/// Read the logical selection on this exact view. The caller must retain this same view when opening physical pages; a fresh physical session is not a substitute for that snapshot.
pub fn selected_generation(
    scope: &DiskANNIndexScope,
    read: &dyn KeyValueRead,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<DiskANNGeneration>> {
    scope.check_control(control)?;
    read.control().check()?;
    let selected = read_head(read, &head_key(scope), control)?;
    if let Some(generation) = selected {
        super::identity::validate_mapping(scope, generation, read, control)?;
        if load_state(read, generation, control)?.map(|state| state.status)
            != Some(DiskANNStageStatus::Published)
        {
            return Err(invalid("selected generation is not published"));
        }
    }
    read.control().check()?;
    control.check()?;
    Ok(selected)
}

fn transition(
    batch: &mut dyn KeyValueBatch,
    generation: DiskANNGeneration,
    state: State,
    status: DiskANNStageStatus,
) -> StorageBackendResult<()> {
    let key = Keys::new(generation).key(Kind::State);
    batch.require_unchanged(key.as_ref())?;
    batch.put(key.as_ref(), &State { status, ..state }.encode())
}

pub(super) fn head_key(scope: &DiskANNIndexScope) -> [u8; HEAD_PREFIX.len() + 48] {
    let mut key = [0; HEAD_PREFIX.len() + 48];
    key[..HEAD_PREFIX.len()].copy_from_slice(HEAD_PREFIX);
    for (i, id) in [scope.table, scope.storage, scope.index].iter().enumerate() {
        let start = HEAD_PREFIX.len() + i * 16;
        key[start..start + 16].copy_from_slice(id);
    }
    key
}

fn encode_head(generation: DiskANNGeneration) -> [u8; HEAD_BYTES] {
    let mut bytes = [0; HEAD_BYTES];
    bytes[0] = 1;
    bytes[1..17].copy_from_slice(&generation.database());
    for (i, value) in [
        generation.table(),
        generation.index(),
        generation.generation(),
    ]
    .iter()
    .enumerate()
    {
        bytes[17 + i * 8..25 + i * 8].copy_from_slice(&value.to_be_bytes());
    }
    bytes
}

fn read_head(
    read: &dyn KeyValueRead,
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<DiskANNGeneration>> {
    fixed::<HEAD_BYTES>(control, |visit| {
        read.visit_value_bounded(key, HEAD_BYTES, control, visit)
    })?
    .map(decode_head)
    .transpose()
}

fn decode_head(bytes: [u8; HEAD_BYTES]) -> StorageBackendResult<DiskANNGeneration> {
    if bytes[0] != 1 {
        return Err(invalid("unsupported generation head revision"));
    }
    let number =
        |offset| u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("fixed head"));
    DiskANNGeneration::new(
        bytes[1..17].try_into().expect("fixed identity"),
        number(17),
        number(25),
        number(33),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diskann_head_uses_fixed_independent_bytes_and_rejects_malformed_records() {
        let generation = DiskANNGeneration::new([17; 16], 0x0102_0304_0506_0708, 9, 10).unwrap();
        let expected = [
            1, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 1, 2, 3, 4, 5, 6, 7,
            8, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 10,
        ];
        assert_eq!(encode_head(generation), expected);
        assert_eq!(decode_head(expected).unwrap(), generation);
        for case in 0..5 {
            let mut bytes = expected;
            match case {
                0 => bytes[0] = 2,
                1 => bytes[1..17].fill(0),
                2 => bytes[17..25].fill(0),
                3 => bytes[25..33].fill(0),
                _ => bytes[33..].fill(0),
            }
            assert!(decode_head(bytes).is_err());
        }
    }
}
