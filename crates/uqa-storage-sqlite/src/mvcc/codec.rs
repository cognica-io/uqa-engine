//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::{params, types::ValueRef, Connection, Row};
use uqa_storage::mvcc::{
    CommitReceipt, CommitSequence, CommitStatus, DatabaseId, StorageTransactionId, VersionError,
};

use super::PhysicalResult;

pub(super) struct Header {
    pub(super) allocated: u64,
    pub(super) sequence: CommitSequence,
    pub(super) key_value_mapping: bool,
}

pub(super) fn bytes<'a>(row: &'a Row<'_>, column: usize) -> PhysicalResult<&'a [u8]> {
    match row.get_ref(column)? {
        ValueRef::Blob(bytes) => Ok(bytes),
        _ => Err(VersionError::InvalidEncoding("expected a record BLOB").into()),
    }
}

pub(super) fn integer(bytes: &[u8]) -> PhysicalResult<u64> {
    Ok(u64::from_be_bytes(bytes.try_into().map_err(|_| {
        VersionError::InvalidEncoding("invalid sequence length")
    })?))
}

pub(super) fn header(connection: &Connection, expected: DatabaseId) -> PhysicalResult<Header> {
    let mut statement = connection.prepare("SELECT format, database_id, allocated, sequence, mapping FROM _uqa_mvcc_metadata WHERE singleton = 1")?;
    let mut rows = statement.query([])?;
    let row = rows
        .next()?
        .ok_or(VersionError::InvalidEncoding("missing record metadata"))?;
    if row.get::<_, i64>(0)? != 32 {
        return Err(VersionError::InvalidEncoding("unknown record format").into());
    }
    let database = identity(bytes(row, 1)?)?;
    if database != expected {
        return Err(VersionError::WrongDatabase.into());
    }
    Ok(Header {
        allocated: integer(bytes(row, 2)?)?,
        sequence: CommitSequence::from_u64(integer(bytes(row, 3)?)?),
        key_value_mapping: match row.get::<_, i64>(4)? {
            0 => false,
            1 => true,
            _ => return Err(VersionError::InvalidEncoding("unknown record mapping").into()),
        },
    })
}

pub(super) fn identity(bytes: &[u8]) -> PhysicalResult<DatabaseId> {
    Ok(DatabaseId::from_bytes(bytes.try_into().map_err(|_| {
        VersionError::InvalidEncoding("invalid database identity")
    })?))
}

pub(super) fn status(
    connection: &Connection,
    transaction: StorageTransactionId,
) -> PhysicalResult<CommitStatus> {
    let mut statement = connection.prepare(
        "SELECT status, sequence, fingerprint FROM _uqa_mvcc_transactions WHERE allocation = ?1",
    )?;
    let id = transaction.allocation().to_be_bytes();
    let mut rows = statement.query(params![id.as_slice()])?;
    let Some(row) = rows.next()? else {
        return Ok(CommitStatus::Unknown);
    };
    match row.get::<_, i64>(0)? {
        0 | 1
            if matches!(row.get_ref(1)?, ValueRef::Null)
                && matches!(row.get_ref(2)?, ValueRef::Null) =>
        {
            Ok(if row.get::<_, i64>(0)? == 0 {
                CommitStatus::Pending
            } else {
                CommitStatus::Aborted
            })
        }
        2 => Ok(CommitStatus::Committed(CommitReceipt {
            transaction,
            sequence: CommitSequence::from_u64(integer(bytes(row, 1)?)?),
            fingerprint: bytes(row, 2)?
                .try_into()
                .map_err(|_| VersionError::InvalidEncoding("invalid commit fingerprint"))?,
        })),
        _ => Err(VersionError::InvalidEncoding("invalid transaction receipt").into()),
    }
}

pub(super) fn head(connection: &Connection, key: &[u8]) -> PhysicalResult<Option<CommitSequence>> {
    if let Some((sequence, _)) = head_state(connection, key)? {
        return Ok(Some(sequence));
    }
    Ok(super::runs::info(connection, key)?.map(|(sequence, _)| sequence))
}

pub(super) fn head_state(
    connection: &Connection,
    key: &[u8],
) -> PhysicalResult<Option<(CommitSequence, bool)>> {
    let mut statement =
        connection.prepare("SELECT sequence, compacted FROM _uqa_mvcc_heads WHERE key = ?1")?;
    let mut rows = statement.query(params![key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    decode_head(row).map(Some)
}

pub(super) fn decode_head(row: &Row<'_>) -> PhysicalResult<(CommitSequence, bool)> {
    let sequence = integer(bytes(row, 0)?)?;
    let compacted: i64 = row.get(1)?;
    if sequence == 0 || !matches!(compacted, 0 | 1) {
        return Err(VersionError::InvalidEncoding("invalid record head").into());
    }
    Ok((CommitSequence::from_u64(sequence), compacted == 1))
}
