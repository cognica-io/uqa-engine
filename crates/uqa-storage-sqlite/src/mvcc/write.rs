//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::{params, Connection};
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::mvcc::{
    resolve_prepared_receipt, CommitFailure, CommitReceipt, CommitResult, CommitSequence,
    CommitStatus, DatabaseId, PreparedRecordCommit, StorageTransactionId, VersionError,
    VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{admission, codec, native, Error, PhysicalResult};

pub(super) fn reserve_bindings(
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> VersionResult<MemoryReservation> {
    let binding = |key: usize, value: u64| {
        usize::try_from(value)
            .ok()
            .and_then(|value| key.checked_mul(2)?.checked_add(value))
            .and_then(|length| length.checked_add(96))
            .ok_or(MemoryError::SizeOverflow)
    };
    let mut peak = 96;
    let mut writes = prepared.writes();
    while let Some(write) = writes.next_metadata(control)? {
        peak = peak.max(binding(write.key().len(), write.value_len().unwrap_or(0))?);
    }
    for key in prepared.required_keys() {
        control.cancellation().check()?;
        peak = peak.max(binding(key.len(), 0)?);
    }
    Ok(control.memory().reserve(peak)?)
}

pub(super) fn allocate(
    connection: &Connection,
    identity: DatabaseId,
    native: Option<native::NativeRecordNamespace>,
    control: &StorageReadControl,
) -> PhysicalResult<StorageTransactionId> {
    allocate_with_owner(connection, identity, native, false, control, |_| Ok(()))
}

pub(super) fn allocate_with_owner(
    connection: &Connection,
    identity: DatabaseId,
    native: Option<native::NativeRecordNamespace>,
    managed: bool,
    control: &StorageReadControl,
    retain: impl FnOnce(StorageTransactionId) -> VersionResult<()>,
) -> PhysicalResult<StorageTransactionId> {
    let mut retain = Some(retain);
    allocate_batch(connection, identity, native, managed, 1, control, |id| {
        retain.take().expect("one allocation")(id)
    })
    .map(|(first, _)| first)
}

/// Every allocation and its liveness lease is installed before one fully
/// synchronized commit. No identifier escapes before its Pending receipt is
/// durable, including identifiers retained for later managed transactions.
pub(super) fn allocate_batch(
    connection: &Connection,
    identity: DatabaseId,
    native: Option<native::NativeRecordNamespace>,
    managed: bool,
    requested: u64,
    control: &StorageReadControl,
    mut retain: impl FnMut(StorageTransactionId) -> VersionResult<()>,
) -> PhysicalResult<(StorageTransactionId, u64)> {
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, native)?;
    let current = codec::header(&transaction, identity)?;
    let requested = requested.min((current.receipt_limit / 4).max(1));
    let count =
        available_receipts(&transaction, &current, requested)?.min(u64::MAX - current.allocated);
    if count == 0 {
        if current.allocated == u64::MAX {
            return Err(VersionError::TransactionIdsExhausted.into());
        }
        return Err(VersionError::ReceiptRetentionExhausted {
            limit: current.receipt_limit,
        }
        .into());
    }
    let first = StorageTransactionId::new(identity, current.allocated + 1)?;
    let last = current.allocated + count;
    transaction
        .prepare_cached("UPDATE _uqa_mvcc_metadata SET allocated = ?1 WHERE singleton = 1")?
        .execute(params![last.to_be_bytes().as_slice()])?;
    {
        let mut statement = transaction.prepare_cached("INSERT INTO _uqa_mvcc_transactions (allocation, status, sequence, fingerprint, managed) VALUES (?1, 0, NULL, NULL, ?2)")?;
        for allocation in first.allocation()..=last {
            let id = StorageTransactionId::new(identity, allocation)?;
            retain(id)?;
            statement.execute(params![
                allocation.to_be_bytes().as_slice(),
                i64::from(managed)
            ])?;
        }
    }
    control.cancellation().check().map_err(VersionError::from)?;
    admission::commit(transaction, control)?;
    Ok((first, count))
}

/// The allocation watermark bounds retained receipts without counting every
/// row. Holes only reduce a batch; count when the bound cannot admit even one.
fn available_receipts(
    connection: &Connection,
    current: &codec::Header,
    requested: u64,
) -> PhysicalResult<u64> {
    let oldest = {
        let mut statement = connection.prepare_cached(
            "SELECT allocation FROM _uqa_mvcc_transactions ORDER BY allocation LIMIT 1",
        )?;
        let mut rows = statement.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(requested.min(current.receipt_limit));
        };
        codec::integer(codec::bytes(row, 0)?)?
    };
    if let Some(available) = current
        .allocated
        .checked_sub(oldest)
        .and_then(|span| current.receipt_limit.checked_sub(span.saturating_add(1)))
        .filter(|available| *available > 0)
    {
        return Ok(requested.min(available));
    }
    let retained: i64 = connection
        .prepare_cached("SELECT count(*) FROM _uqa_mvcc_transactions")?
        .query_row([], |row| row.get(0))?;
    let retained = u64::try_from(retained)
        .map_err(|_| VersionError::InvalidEncoding("negative transaction receipt count"))?;
    Ok(requested.min(current.receipt_limit.saturating_sub(retained)))
}

pub(super) fn commit(
    connection: &Connection,
    id: StorageTransactionId,
    prepared: &PreparedRecordCommit,
    native: Option<native::NativeRecordNamespace>,
    control: &StorageReadControl,
    retain_data_commit: impl FnOnce(CommitReceipt),
) -> CommitResult {
    let rejected = |error: Error| CommitFailure::Rejected(error.into_version());
    let _permit = admission::permit(connection, control).map_err(rejected)?;
    let transaction = admission::begin(connection, control).map_err(rejected)?;
    native::check_mapping(&transaction, native).map_err(rejected)?;
    let current = codec::header(&transaction, id.database()).map_err(rejected)?;
    let status = codec::status(&transaction, id).map_err(rejected)?;
    if let Some(receipt) = resolve_prepared_receipt(status, id, prepared.fingerprint())? {
        return Ok(receipt);
    }
    prepared.validate_snapshot(current.sequence)?;
    super::tombstones::validate(&transaction, prepared, control).map_err(rejected)?;
    prepared.validate(control, |key| {
        codec::head(&transaction, key).map_err(Error::into_version)
    })?;
    let sequence = if prepared.is_empty() {
        current.sequence
    } else {
        current.sequence.successor()?
    };
    let receipt = CommitReceipt {
        transaction: id,
        sequence,
        fingerprint: prepared.fingerprint(),
    };
    let preserves_definitions = if let Some(namespace) = native {
        native::materialize(&transaction, namespace.0, prepared, sequence, control)
            .map_err(rejected)?
    } else {
        false
    };
    stage(&transaction, prepared, receipt, control).map_err(rejected)?;
    admission::commit(transaction, control).map_err(|error| match error {
        Error::Version(error @ VersionError::Cancelled(_)) => CommitFailure::Rejected(error),
        error => CommitFailure::Indeterminate {
            transaction: id,
            source: error.into_version().into_storage_error(),
        },
    })?;
    if preserves_definitions {
        retain_data_commit(receipt);
    }
    Ok(receipt)
}

fn stage(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    receipt: CommitReceipt,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let sequence = receipt.sequence.as_u64().to_be_bytes();
    let has_runs: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)")?
        .query_row([], |row| row.get(0))?;
    {
        let mut versions = connection.prepare_cached(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, ?3)",
        )?;
        let mut previous = connection.prepare_cached("INSERT INTO _uqa_mvcc_versions (key, sequence, value) SELECT key, sequence, NULL FROM _uqa_mvcc_heads WHERE key = ?1 AND compacted = 1")?;
        let mut heads = connection.prepare_cached("INSERT INTO _uqa_mvcc_heads (key, sequence, compacted) VALUES (?1, ?2, 0) ON CONFLICT(key) DO UPDATE SET sequence = excluded.sequence, compacted = 0")?;
        let mut writes = prepared.writes();
        while let Some(write) = writes.next(control)? {
            // Validation found the head each write expects. A key that expects none has no head, so it is in no run and has no compacted tombstone to restore before its first version.
            if write.expected().is_some() {
                if has_runs {
                    super::runs::extract(connection, write.key(), control)?;
                }
                previous.execute([write.key()])?;
                previous.clear_bindings();
            } else {
                debug_assert!(
                    codec::head(connection, write.key())?.is_none(),
                    "a write that expects no revision was staged over a head"
                );
            }
            versions.execute(params![write.key(), sequence.as_slice(), write.value()])?;
            versions.clear_bindings();
            heads.execute(params![write.key(), sequence.as_slice()])?;
            heads.clear_bindings();
        }
    }
    connection
        .prepare_cached("UPDATE _uqa_mvcc_metadata SET sequence = ?1 WHERE singleton = 1")?
        .execute(params![sequence.as_slice()])?;
    connection.prepare_cached("UPDATE _uqa_mvcc_transactions SET status = 2, sequence = ?1, fingerprint = ?2 WHERE allocation = ?3")?.execute(params![sequence.as_slice(), receipt.fingerprint.as_slice(), receipt.transaction.allocation().to_be_bytes().as_slice()])?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

pub(super) fn stage_record(
    connection: &Connection,
    key: &[u8],
    value: Option<&[u8]>,
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _bindings =
        crate::read_control::reserve_bindings(control, &[key, value.unwrap_or_default()])?;
    let sequence = sequence.as_u64().to_be_bytes();
    super::runs::extract(connection, key, control)?;
    connection.prepare_cached("INSERT INTO _uqa_mvcc_versions (key, sequence, value) SELECT key, sequence, NULL FROM _uqa_mvcc_heads WHERE key = ?1 AND compacted = 1")?.execute([key])?;
    connection
        .prepare_cached("INSERT INTO _uqa_mvcc_versions(key, sequence, value) VALUES (?1, ?2, ?3)")?
        .execute(params![key, sequence.as_slice(), value])?;
    connection.prepare_cached("INSERT INTO _uqa_mvcc_heads(key, sequence, compacted) VALUES (?1, ?2, 0) ON CONFLICT(key) DO UPDATE SET sequence = excluded.sequence, compacted = 0")?.execute(params![key, sequence.as_slice()])?;
    Ok(())
}

pub(super) fn abort(
    connection: &Connection,
    id: StorageTransactionId,
    native: Option<native::NativeRecordNamespace>,
    control: &StorageReadControl,
) -> PhysicalResult<CommitStatus> {
    // Recording an abort waits for writer admission like any other write instead of failing once SQLite's busy timeout expires, which would leave the rollback to surface later as a cleanup failure.
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, native)?;
    codec::header(&transaction, id.database())?;
    let status = codec::status(&transaction, id)?;
    if status != CommitStatus::Pending {
        return Ok(status);
    }
    transaction
        .prepare_cached("UPDATE _uqa_mvcc_transactions SET status = 1 WHERE allocation = ?1")?
        .execute(params![id.allocation().to_be_bytes().as_slice()])?;
    control.cancellation().check().map_err(VersionError::from)?;
    admission::commit(transaction, control)?;
    Ok(CommitStatus::Aborted)
}
