//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable identifier watermarks share `SQLite`'s physical commit admission, independently of logical records.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::key_value::diskann_identifiers;
use uqa_storage::mvcc::{
    reserve_identifier_workspace, DatabaseId, IdentifierAllocation, IdentifierRequest,
};
use uqa_storage::read_control::StorageReadControl;

use super::{admission, codec, native, PhysicalResult};

pub(super) const TABLE: (&str, &str) = (
    "_uqa_mvcc_identifiers",
    "CREATE TABLE _uqa_mvcc_identifiers (namespace BLOB PRIMARY KEY CHECK(typeof(namespace) = 'blob' AND length(namespace) > 0), watermark BLOB NOT NULL CHECK(typeof(watermark) = 'blob' AND length(watermark) = 8)) WITHOUT ROWID",
);

/// Called only inside the verified predecessor's atomic initialization transaction and write permit.
pub(super) fn consolidate_diskann_generations(connection: &Connection) -> PhysicalResult<()> {
    use diskann_identifiers::{LEGACY_END, LEGACY_NAMESPACE_BYTES, LEGACY_PREFIX, NAMESPACE};
    let previous = watermark(connection, NAMESPACE)?;
    let mut maximum = previous;
    let mut after: Option<[u8; LEGACY_NAMESPACE_BYTES]> = None;
    loop {
        let mut keys = [[0_u8; LEGACY_NAMESPACE_BYTES]; 64];
        let mut count = 0;
        {
            let mut statement = connection.prepare("SELECT namespace, watermark FROM _uqa_mvcc_identifiers WHERE namespace >= ?1 AND namespace < ?2 AND length(namespace) = ?3 ORDER BY namespace LIMIT 64")?;
            let mut rows = statement.query(params![
                after
                    .as_ref()
                    .map_or(LEGACY_PREFIX.as_slice(), |key| key.as_slice()),
                LEGACY_END.as_slice(),
                LEGACY_NAMESPACE_BYTES as i64,
            ])?;
            while let Some(row) = rows.next()? {
                keys[count].copy_from_slice(codec::bytes(row, 0)?);
                let value = codec::integer(codec::bytes(row, 1)?)?;
                maximum = Some(maximum.map_or(value, |old| old.max(value)));
                count += 1;
            }
        }
        if count == 0 {
            break;
        }
        let mut deletion =
            connection.prepare("DELETE FROM _uqa_mvcc_identifiers WHERE namespace = ?1")?;
        for key in &keys[..count] {
            deletion.execute([key.as_slice()])?;
        }
        after = Some(keys[count - 1]);
    }
    if maximum != previous {
        let value = maximum.expect("only observed reservations change the maximum");
        connection.execute("INSERT INTO _uqa_mvcc_identifiers VALUES (?1,?2) ON CONFLICT(namespace) DO UPDATE SET watermark=excluded.watermark", params![NAMESPACE, value.to_be_bytes().as_slice()])?;
    }
    Ok(())
}

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
    native: Option<native::NativeRecordNamespace>,
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
    native: Option<native::NativeRecordNamespace>,
    namespace: &[u8],
    request: IdentifierRequest,
    control: &StorageReadControl,
) -> PhysicalResult<IdentifierAllocation> {
    let _workspace = request.reserve_workspace(namespace, control)?;
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
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
    admission::commit(transaction, control)?;
    Ok(allocation)
}
