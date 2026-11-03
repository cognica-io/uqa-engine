//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Format zero keeps an unfinished image outside every ordinary and restored record-open path.

use rusqlite::{params, Connection};
use uqa_storage::{
    backup::{BackupRoot, ImageProvider},
    mvcc::{DatabaseRestore, VersionError},
    read_control::StorageReadControl,
};

use super::{
    super::{admission, key_value},
    PhysicalResult, RECORD_FORMAT, TABLES,
};

use super::image_binding::IMAGE;

fn key_value_mapping(root: BackupRoot) -> PhysicalResult<bool> {
    match root.cut().format.provider() {
        ImageProvider::SQLiteKV => Ok(true),
        ImageProvider::SQLiteNative => Ok(false),
        _ => Err(VersionError::InvalidEncoding("non-SQLite image mapping").into()),
    }
}

fn metadata_definition() -> String {
    TABLES[0].1.replace(
        &format!("CHECK(format = {RECORD_FORMAT})"),
        "CHECK(format = 0)",
    )
}

pub(in crate::mvcc) fn initialize(
    connection: &Connection,
    root: BackupRoot,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let _timeout = admission::BusyTimeout::new(connection)?;
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    let empty: bool = transaction.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema)",
        [],
        |row| row.get(0),
    )?;
    if !empty {
        return Err(VersionError::InvalidEncoding("image import destination has a schema").into());
    }
    super::create_tables(&transaction, &metadata_definition())?;
    let key_value = key_value_mapping(root)?;
    if key_value {
        key_value::create_mapping_guards(&transaction)?;
    }
    let sequence = root
        .cut()
        .sequence
        .ok_or(VersionError::InvalidEncoding("missing image sequence"))?;
    transaction.execute(
        "INSERT INTO _uqa_mvcc_metadata (singleton, format, database_id, allocated, sequence, mapping, restore_target, sequence_durable) VALUES (1, 0, ?1, x'0000000000000000', ?2, ?4, ?3, ?2)",
        params![request.source().as_bytes().as_slice(), sequence.as_u64().to_be_bytes().as_slice(), request.target().as_bytes().as_slice(), key_value],
    )?;
    transaction.execute_batch(IMAGE.1)?;
    transaction.execute(
        "INSERT INTO _uqa_backup_import VALUES (1, ?1, ?2)",
        params![
            root.encode().as_slice(),
            request.target().as_bytes().as_slice()
        ],
    )?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        transaction.execute_batch(&super::trigger(IMAGE.0, action).1)?;
    }
    admission::commit(transaction, control)
}

/// Called only after complete repository verification, inside the still exclusive import owner. The canonical format and retained intent become visible in one transaction; the existing history owner then publishes the auxiliary coordinator and new identity.
pub(in crate::mvcc) fn complete(
    connection: &Connection,
    root: BackupRoot,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let _timeout = admission::BusyTimeout::new(connection)?;
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    let matches: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM _uqa_backup_import WHERE singleton = 1 AND root = ?1 AND target = ?2) AND EXISTS(SELECT 1 FROM _uqa_mvcc_metadata WHERE singleton = 1 AND format = 0 AND database_id = ?3 AND restore_target = ?2 AND mapping = ?5 AND allocated = x'0000000000000000' AND sequence = ?4 AND sequence_durable = sequence)",
        params![root.encode().as_slice(), request.target().as_bytes().as_slice(), request.source().as_bytes().as_slice(), root.cut().sequence.unwrap().as_u64().to_be_bytes().as_slice(), key_value_mapping(root)?],
        |row| row.get(0),
    )?;
    if !matches {
        return Err(VersionError::InvalidEncoding("image import binding changed").into());
    }
    transaction
        .execute_batch("ALTER TABLE _uqa_mvcc_metadata RENAME TO _uqa_mvcc_import_metadata")?;
    transaction.execute_batch(TABLES[0].1)?;
    transaction.execute_batch(&format!("INSERT INTO _uqa_mvcc_metadata SELECT singleton, {RECORD_FORMAT}, database_id, allocated, sequence, mapping, restore_target, receipt_limit, sequence_durable FROM _uqa_mvcc_import_metadata; DROP TABLE _uqa_mvcc_import_metadata;"))?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        transaction.execute_batch(&super::trigger(TABLES[0].0, action).1)?;
    }
    admission::commit(transaction, control)
}
