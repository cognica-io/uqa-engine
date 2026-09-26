//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Quiescent tombstone retirement and absence epochs share one physical transaction.

use rusqlite::{params, Connection};
use uqa_storage::mvcc::{
    CommitSequence, DatabaseId, PreparedRecordCommit, TombstoneReclamationPage,
    TombstoneReclamationRequest, TombstoneReclamationStep, VersionError, RECLAMATION_DOMAIN_PREFIX,
    RECLAMATION_EPOCH_NAMESPACE, TOMBSTONE_RECLAMATION_PAGE,
};
use uqa_storage::read_control::StorageReadControl;

use super::{admission, codec, identifiers, native, read, runs, PhysicalResult};

pub(super) fn epoch(connection: &Connection) -> PhysicalResult<u64> {
    Ok(identifiers::watermark(connection, RECLAMATION_EPOCH_NAMESPACE)?.unwrap_or(0))
}

pub(super) fn validate(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let current = epoch(connection)?;
    let upper = crate::read_control::prefix_upper_bound(RECLAMATION_DOMAIN_PREFIX, control)?;
    let _bindings = crate::read_control::reserve_bindings(
        control,
        &[
            RECLAMATION_DOMAIN_PREFIX,
            upper.as_deref().expect("finite namespace"),
        ],
    )?;
    let mut statement = connection.prepare("SELECT namespace, watermark FROM _uqa_mvcc_identifiers WHERE namespace >= ?1 AND namespace < ?2 ORDER BY namespace")?;
    let mut rows = statement.query(params![RECLAMATION_DOMAIN_PREFIX, upper.as_deref()])?;
    while let Some(row) = rows.next()? {
        control.check().map_err(VersionError::from)?;
        let namespace = codec::bytes(row, 0)?;
        let prefix = &namespace[RECLAMATION_DOMAIN_PREFIX.len()..];
        if prefix.len() > 1024 {
            return Err(VersionError::InvalidEncoding("oversized reclamation domain").into());
        }
        prepared.validate_reclamation_epoch(
            prefix,
            codec::integer(codec::bytes(row, 1)?)?,
            current,
            control.cancellation(),
        )?;
    }
    Ok(())
}

pub(super) fn reclaim(
    connection: &Connection,
    identity: DatabaseId,
    mapped: Option<native::NativeRecordNamespace>,
    request: &TombstoneReclamationRequest<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<TombstoneReclamationStep> {
    request.validate(control)?;
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, mapped)?;
    let current = codec::header(&transaction, identity)?.sequence;
    if request.through > current {
        return Err(
            VersionError::InvalidEncoding("tombstone cutoff exceeds committed visibility").into(),
        );
    }
    let page = candidates(&transaction, request, control)?;
    if page.retired().len() != 0 {
        let next = epoch(&transaction)?
            .checked_add(1)
            .ok_or(VersionError::IdentifiersExhausted)?;
        let namespace = request.domain_namespace(control)?;
        for (key, _) in page.retired() {
            control.check().map_err(VersionError::from)?;
            let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
            runs::extract(&transaction, key, control)?;
            transaction.execute("DELETE FROM _uqa_mvcc_versions WHERE key = ?1", [key])?;
            transaction.execute("DELETE FROM _uqa_mvcc_heads WHERE key = ?1", [key])?;
        }
        let value = next.to_be_bytes();
        let _bindings = crate::read_control::reserve_bindings(control, &[&namespace, &value])?;
        for key in [RECLAMATION_EPOCH_NAMESPACE, &*namespace] {
            transaction.execute("INSERT INTO _uqa_mvcc_identifiers VALUES (?1, ?2) ON CONFLICT(namespace) DO UPDATE SET watermark = excluded.watermark", params![key, value.as_slice()])?;
        }
    }
    control.check().map_err(VersionError::from)?;
    admission::commit(transaction, control)?;
    Ok(page.finish())
}

fn candidates(
    connection: &Connection,
    request: &TombstoneReclamationRequest<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<TombstoneReclamationPage> {
    let mut page = TombstoneReclamationPage::new(control)?;
    read::keys(
        connection,
        request.prefix,
        request.after,
        TOMBSTONE_RECLAMATION_PAGE,
        control,
        &mut |key| {
            let current = read::info(connection, key, CommitSequence::from_u64(u64::MAX))?.ok_or(
                VersionError::InvalidEncoding("reclamation record disappeared"),
            )?;
            if current.revision > request.through.as_u64() {
                return Ok(None);
            }
            let retire = current.length.is_none()
                && fully_pruned(connection, key, current.revision, control)?;
            page.visit(key, current.revision, retire, control)?;
            Ok(Some(true))
        },
    )?;
    Ok(page)
}

fn fully_pruned(
    connection: &Connection,
    key: &[u8],
    revision: u64,
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
    let mut statement = connection.prepare("SELECT sequence, value IS NULL FROM _uqa_mvcc_versions WHERE key = ?1 ORDER BY sequence LIMIT 2")?;
    let mut rows = statement.query([key])?;
    let Some(row) = rows.next()? else {
        return Ok(true);
    };
    let sequence = codec::integer(codec::bytes(row, 0)?)?;
    let deleted: bool = row.get(1)?;
    if rows.next()?.is_some() {
        return Ok(false);
    }
    if sequence != revision {
        return Ok(false);
    }
    if !deleted {
        return Err(VersionError::InvalidEncoding("compacted head has a live version").into());
    }
    Ok(true)
}
