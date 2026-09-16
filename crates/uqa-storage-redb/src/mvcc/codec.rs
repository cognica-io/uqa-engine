//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical versioned-record metadata and receipt encodings.

use redb::ReadableTable;
use uqa_storage::mvcc::{
    CommitReceipt, CommitSequence, CommitStatus, DatabaseId, StorageTransactionId, VersionError,
    VersionResult,
};

use crate::error::redb_error;

pub(super) fn database_id(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
) -> VersionResult<DatabaseId> {
    let bytes = table
        .get("database")
        .map_err(redb_error)?
        .ok_or(VersionError::InvalidEncoding("missing database identity"))?;
    Ok(DatabaseId::from_bytes(bytes.value().try_into().map_err(
        |_| VersionError::InvalidEncoding("invalid database identity"),
    )?))
}

pub(super) fn read_u64(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> VersionResult<u64> {
    let bytes = table
        .get(key)
        .map_err(redb_error)?
        .ok_or(VersionError::InvalidEncoding("missing MVCC metadata"))?;
    decode_u64(bytes.value())
}

pub(super) fn decode_u64(bytes: &[u8]) -> VersionResult<u64> {
    Ok(u64::from_be_bytes(bytes.try_into().map_err(|_| {
        VersionError::InvalidEncoding("invalid integer width")
    })?))
}

pub(super) fn status(
    table: &impl ReadableTable<u64, &'static [u8]>,
    transaction: StorageTransactionId,
) -> VersionResult<CommitStatus> {
    let Some(bytes) = table.get(transaction.allocation()).map_err(redb_error)? else {
        return Ok(CommitStatus::Unknown);
    };
    let bytes = bytes.value();
    match bytes {
        [0] => Ok(CommitStatus::Pending),
        [1] => Ok(CommitStatus::Aborted),
        [2, rest @ ..] if rest.len() == 40 => Ok(CommitStatus::Committed(CommitReceipt {
            transaction,
            sequence: CommitSequence::from_u64(decode_u64(&rest[..8])?),
            fingerprint: rest[8..].try_into().expect("checked receipt width"),
        })),
        _ => Err(VersionError::InvalidEncoding("invalid transaction receipt")),
    }
}

pub(super) fn receipt_bytes(receipt: CommitReceipt) -> [u8; 41] {
    let mut bytes = [0; 41];
    bytes[0] = 2;
    bytes[1..9].copy_from_slice(&receipt.sequence.as_u64().to_be_bytes());
    bytes[9..].copy_from_slice(&receipt.fingerprint);
    bytes
}

pub(super) fn value_bytes(encoded: &[u8]) -> VersionResult<Option<&[u8]>> {
    match encoded {
        [0] => Ok(None),
        [1, value @ ..] => Ok(Some(value)),
        _ => Err(VersionError::InvalidEncoding("invalid record payload")),
    }
}
