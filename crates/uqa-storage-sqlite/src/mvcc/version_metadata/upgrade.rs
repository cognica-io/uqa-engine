//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic conversion of keyed payloads to stable physical addresses.

use super::{
    create_triggers, remove_metadata, schema, Connection, PhysicalResult, TABLE, VERSIONS_TABLE,
};
#[cfg(test)]
use super::{PREVIOUS_TABLE, PREVIOUS_TRIGGERS, PREVIOUS_VERSIONS_TABLE};

pub(in crate::mvcc) fn upgrade(connection: &Connection, has_metadata: bool) -> PhysicalResult<()> {
    if has_metadata {
        remove_metadata(connection)?;
    }
    connection
        .execute_batch("ALTER TABLE _uqa_mvcc_versions RENAME TO _uqa_mvcc_previous_versions")?;
    connection.execute_batch(VERSIONS_TABLE.1)?;
    connection.execute_batch(TABLE.1)?;
    guards(connection, TABLE.0)?;
    create_triggers(connection)?;
    // SQLite streams the predecessor into the new row store. Every source tuple
    // keeps its logical identity and revision, including tombstones; no Rust
    // collection or payload sort is needed. The caller owns the transaction.
    connection.execute_batch(
        "INSERT INTO _uqa_mvcc_versions (key, sequence, value) SELECT key, sequence, value FROM _uqa_mvcc_previous_versions;
         DROP TABLE _uqa_mvcc_previous_versions;",
    )?;
    guards(connection, VERSIONS_TABLE.0)
}

fn guards(connection: &Connection, table: &str) -> PhysicalResult<()> {
    for action in ["INSERT", "UPDATE", "DELETE"] {
        connection.execute_batch(&schema::trigger(table, action).1)?;
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::mvcc) fn install_predecessor(
    connection: &Connection,
    format: i64,
) -> PhysicalResult<()> {
    remove_metadata(connection)?;
    connection
        .execute_batch("ALTER TABLE _uqa_mvcc_versions RENAME TO _uqa_mvcc_addressed_versions")?;
    connection.execute_batch(PREVIOUS_VERSIONS_TABLE)?;
    connection.execute_batch(
        "INSERT INTO _uqa_mvcc_versions SELECT key, sequence, value FROM _uqa_mvcc_addressed_versions;
         DROP TABLE _uqa_mvcc_addressed_versions;",
    )?;
    guards(connection, VERSIONS_TABLE.0)?;
    if format >= 55 {
        connection.execute_batch(PREVIOUS_TABLE)?;
        guards(connection, TABLE.0)?;
        connection.execute_batch("INSERT INTO _uqa_mvcc_version_metadata SELECT key, sequence, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _uqa_mvcc_versions")?;
        for (_, definition) in PREVIOUS_TRIGGERS {
            connection.execute_batch(definition)?;
        }
    }
    Ok(())
}
