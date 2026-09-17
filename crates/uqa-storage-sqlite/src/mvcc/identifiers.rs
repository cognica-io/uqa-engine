//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable identifier watermarks share `SQLite`'s physical commit admission, independently of logical records.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::mvcc::{
    reserve_identifier_workspace, DatabaseId, IdentifierAllocation, IdentifierRequest,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, native, schema, PhysicalResult};

pub(super) const TABLE: (&str, &str) = (
    "_uqa_mvcc_identifiers",
    "CREATE TABLE _uqa_mvcc_identifiers (namespace BLOB PRIMARY KEY CHECK(typeof(namespace) = 'blob' AND length(namespace) > 0), watermark BLOB NOT NULL CHECK(typeof(watermark) = 'blob' AND length(watermark) = 8)) WITHOUT ROWID",
);

fn watermark(connection: &Connection, namespace: &[u8]) -> PhysicalResult<Option<u64>> {
    connection
        .query_row(
            "SELECT watermark FROM _uqa_mvcc_identifiers WHERE namespace = ?1",
            [namespace],
            |row| Ok(codec::bytes(row, 0).and_then(codec::integer)),
        )
        .optional()?
        .transpose()
}

pub(super) fn read(
    connection: &Connection,
    database: DatabaseId,
    native: bool,
    namespace: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<Option<u64>> {
    let _workspace = reserve_identifier_workspace(namespace, control)?;
    let transaction = connection.unchecked_transaction()?;
    native::check_mapping(&transaction, native)?;
    codec::header(&transaction, database)?;
    let result = watermark(&transaction, namespace)?;
    control
        .cancellation()
        .check()
        .map_err(uqa_storage::mvcc::VersionError::from)?;
    transaction.commit()?;
    Ok(result)
}

pub(super) fn allocate(
    connection: &Connection,
    database: DatabaseId,
    native: bool,
    namespace: &[u8],
    request: IdentifierRequest,
    control: &StorageReadControl,
) -> PhysicalResult<IdentifierAllocation> {
    let _workspace = request.reserve_workspace(namespace, control)?;
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
    native::check_mapping(&transaction, native)?;
    codec::header(&transaction, database)?;
    let previous = watermark(&transaction, namespace)?;
    let allocation = request.prepare(previous)?;
    if previous != Some(allocation.watermark()) {
        transaction.execute(
            "INSERT INTO _uqa_mvcc_identifiers VALUES (?1, ?2) ON CONFLICT(namespace) DO UPDATE SET watermark = excluded.watermark",
            params![namespace, allocation.watermark().to_be_bytes().as_slice()],
        )?;
    }
    control
        .cancellation()
        .check()
        .map_err(uqa_storage::mvcc::VersionError::from)?;
    transaction.commit()?;
    Ok(allocation)
}
