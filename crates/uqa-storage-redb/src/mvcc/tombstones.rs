//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retired record identities and their observation floor share one redb commit.

use std::ops::Bound::{Excluded, Included, Unbounded};

use redb::ReadableTable;
use uqa_storage::mvcc::{
    CommitSequence, PreparedRecordCommit, TombstoneReclamationPage, TombstoneReclamationRequest,
    TombstoneReclamationStep, VersionError, VersionResult, RECLAMATION_DOMAIN_PREFIX,
    RECLAMATION_EPOCH_NAMESPACE,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, identifiers, physical_writer, RedbRecordStore, HEADS, METADATA, VERSIONS};
use crate::error::redb_error;

pub(super) fn epoch(
    identifiers: &impl ReadableTable<&'static [u8], &'static [u8]>,
) -> VersionResult<u64> {
    identifiers
        .get(RECLAMATION_EPOCH_NAMESPACE)
        .map_err(redb_error)?
        .map(|value| codec::decode_u64(value.value()))
        .transpose()
        .map(|value| value.unwrap_or(0))
}

pub(super) fn validate(
    identifiers: &impl ReadableTable<&'static [u8], &'static [u8]>,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let current = epoch(identifiers)?;
    for entry in identifiers
        .range(RECLAMATION_DOMAIN_PREFIX..)
        .map_err(redb_error)?
    {
        control.check()?;
        let (key, value) = entry.map_err(redb_error)?;
        let Some(prefix) = key.value().strip_prefix(RECLAMATION_DOMAIN_PREFIX) else {
            break;
        };
        if prefix.len() > 1024 {
            return Err(VersionError::InvalidEncoding(
                "oversized reclamation domain",
            ));
        }
        prepared.validate_reclamation_epoch(
            prefix,
            codec::decode_u64(value.value())?,
            current,
            control.cancellation(),
        )?;
    }
    Ok(())
}

pub(super) fn reclaim(
    store: &RedbRecordStore,
    request: &TombstoneReclamationRequest<'_>,
    control: &StorageReadControl,
) -> VersionResult<TombstoneReclamationStep> {
    request.validate(control)?;
    let transaction = physical_writer(&store.database)?;
    let result = {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        let current = CommitSequence::from_u64(codec::read_u64(&metadata, "sequence")?);
        if request.through > current {
            return Err(VersionError::InvalidEncoding(
                "tombstone cutoff exceeds committed visibility",
            ));
        }
        let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        let page = candidates(&heads, &versions, request, control)?;
        if page.retired().len() != 0 {
            let mut identifiers = transaction
                .open_table(identifiers::TABLE)
                .map_err(redb_error)?;
            let next = epoch(&identifiers)?
                .checked_add(1)
                .ok_or(VersionError::IdentifiersExhausted)?;
            let domain = request.domain_namespace(control)?;
            for (key, revision) in page.retired() {
                control.check()?;
                versions.remove((key, revision)).map_err(redb_error)?;
                heads.remove(key).map_err(redb_error)?;
            }
            for namespace in [RECLAMATION_EPOCH_NAMESPACE, &*domain] {
                identifiers
                    .insert(namespace, next.to_be_bytes().as_slice())
                    .map_err(redb_error)?;
            }
        }
        page.finish()
    };
    control.check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(result)
}

fn candidates(
    heads: &impl ReadableTable<&'static [u8], u64>,
    versions: &impl ReadableTable<(&'static [u8], u64), &'static [u8]>,
    request: &TombstoneReclamationRequest<'_>,
    control: &StorageReadControl,
) -> VersionResult<TombstoneReclamationPage> {
    let mut page = TombstoneReclamationPage::new(control)?;
    let start = request.after.map_or(Included(request.prefix), Excluded);
    for entry in heads
        .range::<&[u8]>((start, Unbounded))
        .map_err(redb_error)?
    {
        control.check()?;
        let (key, head) = entry.map_err(redb_error)?;
        let key = key.value();
        if !key.starts_with(request.prefix) {
            break;
        }
        if head.value() > request.through.as_u64() {
            continue;
        }
        let mut history = versions
            .range((key, 0)..=(key, u64::MAX))
            .map_err(redb_error)?;
        let (identity, value) = history
            .next()
            .transpose()
            .map_err(redb_error)?
            .ok_or(VersionError::InvalidEncoding("record head has no history"))?;
        let mut retire = false;
        if history.next().transpose().map_err(redb_error)?.is_none() {
            if identity.value().1 != head.value() {
                return Err(VersionError::InvalidEncoding(
                    "record head has no matching history",
                ));
            }
            retire = codec::value_bytes(value.value())?.is_none();
        }
        page.visit(key, head.value(), retire, control)?;
        if page.is_full() {
            break;
        }
    }
    Ok(page)
}
