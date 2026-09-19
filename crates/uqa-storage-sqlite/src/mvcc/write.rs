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

use super::{admission, codec, native, schema, Error, PhysicalResult};

pub(super) fn reserve_bindings(
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> VersionResult<MemoryReservation> {
    let mut peak = 96;
    for (key, value) in prepared
        .records()
        .iter()
        .map(|record| (record.key(), record.value()))
        .chain(prepared.required_keys().map(|key| (key, None)))
    {
        control.cancellation().check()?;
        let bytes = key
            .len()
            .checked_mul(2)
            .and_then(|key| key.checked_add(value.map_or(0, <[u8]>::len)))
            .and_then(|length| length.checked_add(96))
            .ok_or(MemoryError::SizeOverflow)?;
        peak = peak.max(bytes);
    }
    Ok(control.memory().reserve(peak)?)
}

pub(super) fn allocate(
    connection: &Connection,
    identity: DatabaseId,
    native: bool,
    control: &StorageReadControl,
) -> PhysicalResult<StorageTransactionId> {
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, native)?;
    let current = codec::header(&transaction, identity)?;
    let id = StorageTransactionId::new(
        identity,
        current
            .allocated
            .checked_add(1)
            .ok_or(VersionError::TransactionIdsExhausted)?,
    )?;
    let bytes = id.allocation().to_be_bytes();
    transaction.execute(
        "UPDATE _uqa_mvcc_metadata SET allocated = ?1 WHERE singleton = 1",
        params![bytes.as_slice()],
    )?;
    transaction.execute(
        "INSERT INTO _uqa_mvcc_transactions VALUES (?1, 0, NULL, NULL)",
        params![bytes.as_slice()],
    )?;
    control.cancellation().check().map_err(VersionError::from)?;
    admission::commit(transaction, control)?;
    Ok(id)
}

pub(super) fn commit(
    connection: &Connection,
    id: StorageTransactionId,
    prepared: &PreparedRecordCommit,
    native: bool,
    control: &StorageReadControl,
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
    prepared.validate(control.cancellation(), |key| {
        codec::head(&transaction, key).map_err(Error::into_version)
    })?;
    let sequence = if prepared.records().is_empty() {
        current.sequence
    } else {
        current.sequence.successor()?
    };
    let receipt = CommitReceipt {
        transaction: id,
        sequence,
        fingerprint: prepared.fingerprint(),
    };
    if native {
        native::materialize(&transaction, id.database(), prepared, sequence, control)
            .map_err(rejected)?;
    }
    stage(&transaction, prepared, receipt, control).map_err(rejected)?;
    admission::commit(transaction, control).map_err(|error| match error {
        Error::Version(error @ VersionError::Cancelled(_)) => CommitFailure::Rejected(error),
        error => CommitFailure::Indeterminate {
            transaction: id,
            source: error.into_version().into_storage_error(),
        },
    })?;
    Ok(receipt)
}

fn stage(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    receipt: CommitReceipt,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let sequence = receipt.sequence.as_u64().to_be_bytes();
    {
        let mut versions = connection
            .prepare("INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, ?3)")?;
        let mut heads = connection.prepare("INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET sequence = excluded.sequence")?;
        for write in prepared.records() {
            control.cancellation().check().map_err(VersionError::from)?;
            versions.execute(params![write.key(), sequence.as_slice(), write.value()])?;
            versions.clear_bindings();
            heads.execute(params![write.key(), sequence.as_slice()])?;
            heads.clear_bindings();
        }
    }
    connection.execute(
        "UPDATE _uqa_mvcc_metadata SET sequence = ?1 WHERE singleton = 1",
        params![sequence.as_slice()],
    )?;
    connection.execute("UPDATE _uqa_mvcc_transactions SET status = 2, sequence = ?1, fingerprint = ?2 WHERE allocation = ?3", params![sequence.as_slice(), receipt.fingerprint.as_slice(), receipt.transaction.allocation().to_be_bytes().as_slice()])?;
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
    connection.execute(
        "INSERT INTO _uqa_mvcc_versions(key, sequence, value) VALUES (?1, ?2, ?3)",
        params![key, sequence.as_slice(), value],
    )?;
    connection.execute("INSERT INTO _uqa_mvcc_heads(key, sequence) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET sequence = excluded.sequence", params![key, sequence.as_slice()])?;
    Ok(())
}

pub(super) fn abort(
    connection: &Connection,
    id: StorageTransactionId,
    native: bool,
    control: &StorageReadControl,
) -> PhysicalResult<CommitStatus> {
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
    native::check_mapping(&transaction, native)?;
    codec::header(&transaction, id.database())?;
    let status = codec::status(&transaction, id)?;
    if status != CommitStatus::Pending {
        return Ok(status);
    }
    transaction.execute(
        "UPDATE _uqa_mvcc_transactions SET status = 1 WHERE allocation = ?1",
        params![id.allocation().to_be_bytes().as_slice()],
    )?;
    control.cancellation().check().map_err(VersionError::from)?;
    transaction.commit()?;
    Ok(CommitStatus::Aborted)
}
