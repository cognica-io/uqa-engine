//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::{params, Connection};
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::mvcc::{
    resolve_prepared_receipt, CommitFailure, CommitReceipt, CommitResult, CommitStatus, DatabaseId,
    PreparedRecordCommit, StorageTransactionId, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, schema, Error, PhysicalResult};

pub(super) fn reserve_bindings(
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> VersionResult<MemoryReservation> {
    let mut peak = 96;
    for record in prepared.records() {
        control.cancellation().check()?;
        let bytes = record
            .key()
            .len()
            .checked_mul(2)
            .and_then(|key| key.checked_add(record.value().map_or(0, <[u8]>::len)))
            .and_then(|length| length.checked_add(96))
            .ok_or(MemoryError::SizeOverflow)?;
        peak = peak.max(bytes);
    }
    Ok(control.memory().reserve(peak)?)
}

pub(super) fn allocate(
    connection: &Connection,
    identity: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<StorageTransactionId> {
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
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
    transaction.commit()?;
    Ok(id)
}

pub(super) fn commit(
    connection: &Connection,
    id: StorageTransactionId,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> CommitResult {
    let rejected = |error: Error| CommitFailure::Rejected(error.into_version());
    let _permit = schema::WritePermit::acquire(connection).map_err(rejected)?;
    let transaction = schema::begin(connection).map_err(rejected)?;
    let current = codec::header(&transaction, id.database()).map_err(rejected)?;
    let status = codec::status(&transaction, id).map_err(rejected)?;
    if let Some(receipt) = resolve_prepared_receipt(status, id, prepared.fingerprint())? {
        return Ok(receipt);
    }
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
    stage(&transaction, prepared, receipt, control).map_err(rejected)?;
    transaction
        .commit()
        .map_err(|error| CommitFailure::Indeterminate {
            transaction: id,
            source: crate::SQLiteError::SQLite(error).into(),
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

pub(super) fn abort(
    connection: &Connection,
    id: StorageTransactionId,
    control: &StorageReadControl,
) -> PhysicalResult<CommitStatus> {
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
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
