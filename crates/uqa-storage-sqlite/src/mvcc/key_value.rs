//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic migration of the legacy byte store and its old writable namespace.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::mvcc::{CommitSequence, DatabaseId, VersionError};
use uqa_storage::read_control::StorageReadControl;

use crate::read_control::{copy_bytes, payload_length, reserve_bindings};

use super::{codec, schema, PhysicalResult};

const GUARD: &str = "CREATE VIEW _key_value AS SELECT 1 AS versioned_storage_format";
const CATALOG_GUARD: &str =
    "CREATE VIEW _metadata AS SELECT 'storage_kind' AS key, 'key_value_mvcc' AS value";

pub(super) fn initialize(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<DatabaseId> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
    super::native::reject_mapped(&transaction)?;
    let legacy: Option<i64> = transaction
        .query_row(
            "SELECT CASE type WHEN 'table' THEN 1 WHEN 'view' THEN 2 ELSE 3 END FROM sqlite_schema WHERE name = '_key_value'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let (identity, _) = schema::initialize_in(&transaction)?;
    let header = codec::header(&transaction, identity)?;
    if header.key_value_mapping {
        if legacy != Some(2)
            || schema::definition_matches(&transaction, "_key_value", GUARD)? != Some(true)
            || schema::definition_matches(&transaction, "_metadata", CATALOG_GUARD)? != Some(true)
        {
            return Err(
                VersionError::InvalidEncoding("incomplete versioned KeyValue format").into(),
            );
        }
        return Ok(identity);
    }
    if legacy.is_some_and(|kind| kind != 1) {
        return Err(VersionError::InvalidEncoding("unexpected legacy KeyValue object").into());
    }
    let unrelated: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN ('_metadata', '_meta') OR (type = 'table' AND name NOT IN ('_key_value', '_uqa_mvcc_metadata', '_uqa_mvcc_heads', '_uqa_mvcc_versions', '_uqa_mvcc_transactions') AND name NOT GLOB 'sqlite_*'))",
        [],
        |row| row.get(0),
    )?;
    if unrelated {
        return Err(VersionError::InvalidEncoding(
            "native or unrelated SQLite tables require their own record mapping",
        )
        .into());
    }
    if legacy.is_some() {
        validate_legacy(&transaction)?;
        let current = header.sequence;
        if let Some(sequence) = copy_records(&transaction, current, control)? {
            transaction.execute(
                "UPDATE _uqa_mvcc_metadata SET sequence = ?1 WHERE singleton = 1",
                params![sequence.as_u64().to_be_bytes().as_slice()],
            )?;
        }
        transaction.execute_batch("DROP TABLE _key_value")?;
    }
    transaction.execute_batch(GUARD)?;
    transaction.execute_batch(CATALOG_GUARD)?;
    transaction.execute_batch("UPDATE _uqa_mvcc_metadata SET mapping = 1 WHERE singleton = 1")?;
    control.cancellation().check().map_err(VersionError::from)?;
    transaction.commit()?;
    Ok(identity)
}

fn validate_legacy(connection: &Connection) -> PhysicalResult<()> {
    let columns: bool = connection.query_row(
        "SELECT count(*) = 2 AND sum(name = 'key' AND upper(type) = 'BLOB' AND pk = 1 AND \"notnull\" = 1) = 1 AND sum(name = 'value' AND upper(type) = 'BLOB' AND pk = 0 AND \"notnull\" = 1) = 1 FROM pragma_table_info('_key_value')",
        [],
        |row| row.get(0),
    )?;
    if !columns {
        return Err(VersionError::InvalidEncoding("unexpected legacy KeyValue columns").into());
    }
    Ok(())
}

fn copy_records(
    connection: &Connection,
    current: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<Option<CommitSequence>> {
    let mut cursor: Option<BudgetedVec<u8>> = None;
    let mut sequence = None;
    loop {
        control.cancellation().check().map_err(VersionError::from)?;
        let _bindings = reserve_bindings(control, &[cursor.as_deref().unwrap_or_default()])?;
        let (sizes, keys) = if cursor.is_some() {
            ("SELECT length(key), length(value), typeof(key) = 'blob' AND typeof(value) = 'blob' FROM _key_value WHERE key > ?1 ORDER BY key LIMIT 1", "SELECT key FROM _key_value WHERE key > ?1 ORDER BY key LIMIT 1")
        } else {
            ("SELECT length(key), length(value), typeof(key) = 'blob' AND typeof(value) = 'blob' FROM _key_value WHERE ?1 IS NULL ORDER BY key LIMIT 1", "SELECT key FROM _key_value WHERE ?1 IS NULL ORDER BY key LIMIT 1")
        };
        let size: Option<(i64, i64, bool)> = connection
            .query_row(sizes, params![cursor.as_deref()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let Some((key_len, value_len, true)) = size else {
            if size.is_some() {
                return Err(VersionError::InvalidEncoding(
                    "legacy KeyValue payload must be a BLOB",
                )
                .into());
            }
            return Ok(sequence);
        };
        let bytes = payload_length(key_len)?
            .checked_mul(2)
            .and_then(|keys| keys.checked_add(usize::try_from(value_len).ok()?))
            .ok_or(MemoryError::SizeOverflow)
            .map_err(VersionError::from)?;
        let _payload = control
            .memory()
            .reserve(bytes)
            .map_err(VersionError::from)?;
        let key = {
            let mut statement = connection.prepare(keys)?;
            let mut rows = statement.query(params![cursor.as_deref()])?;
            let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
                "legacy key disappeared during migration",
            ))?;
            copy_bytes(codec::bytes(row, 0)?, 0, control)?
        };
        let revision = match sequence {
            Some(revision) => revision,
            None => current.successor()?,
        };
        let encoded = revision.as_u64().to_be_bytes();
        connection.execute(
            "INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, ?2)",
            params![&*key, encoded.as_slice()],
        )?;
        connection.execute(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) SELECT key, ?2, value FROM _key_value WHERE key = ?1",
            params![&*key, encoded.as_slice()],
        )?;
        sequence = Some(revision);
        cursor = Some(key);
    }
}
