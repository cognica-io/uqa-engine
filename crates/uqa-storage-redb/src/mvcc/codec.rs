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

pub(super) fn validate_metadata(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    expected: DatabaseId,
) -> VersionResult<()> {
    if read_u64(table, "format")? != 44 {
        return Err(VersionError::InvalidEncoding("unknown record format"));
    }
    if database_id(table)? != expected {
        return Err(VersionError::WrongDatabase);
    }
    receipt_limit(table)?;
    Ok(())
}

pub(super) fn receipt_limit(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
) -> VersionResult<u64> {
    let limit = read_u64(table, "receipt_limit")?;
    if limit == 0 {
        return Err(VersionError::InvalidEncoding(
            "invalid receipt retention limit",
        ));
    }
    Ok(limit)
}

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
    decode_status(transaction, bytes.value())
}

pub(super) const MANAGED_RECEIPT: u8 = 0x08;

pub(super) fn receipt_tag(bytes: &[u8]) -> VersionResult<u8> {
    let Some(&tag) = bytes.first() else {
        return Err(VersionError::InvalidEncoding("invalid transaction receipt"));
    };
    let status = tag & !MANAGED_RECEIPT;
    if !matches!((status, bytes.len()), (0 | 1 | 3, 1) | (2 | 4, 41)) {
        return Err(VersionError::InvalidEncoding("invalid transaction receipt"));
    }
    Ok(tag)
}

pub(super) fn receipt_owner(
    table: &impl ReadableTable<u64, &'static [u8]>,
    transaction: StorageTransactionId,
) -> VersionResult<u8> {
    let bytes = table
        .get(transaction.allocation())
        .map_err(redb_error)?
        .ok_or(VersionError::UnknownTransaction)?;
    Ok(receipt_tag(bytes.value())? & MANAGED_RECEIPT)
}

pub(super) fn decode_status(
    transaction: StorageTransactionId,
    bytes: &[u8],
) -> VersionResult<CommitStatus> {
    match receipt_tag(bytes)? & !MANAGED_RECEIPT {
        0 => Ok(CommitStatus::Pending),
        1 | 3 => Ok(CommitStatus::Aborted),
        2 | 4 => Ok(CommitStatus::Committed(CommitReceipt {
            transaction,
            sequence: CommitSequence::from_u64(decode_u64(&bytes[1..9])?),
            fingerprint: bytes[9..].try_into().expect("checked receipt width"),
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
