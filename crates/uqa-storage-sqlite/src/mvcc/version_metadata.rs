//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload-free predecessor pages maintained in the same transaction as their versions.

use rusqlite::Connection;

use super::{schema, PhysicalResult, VersionError};

pub(super) const TABLE: (&str, &str) = (
    "_uqa_mvcc_version_metadata",
    "CREATE TABLE _uqa_mvcc_version_metadata (key BLOB NOT NULL, sequence BLOB NOT NULL, payload_length INTEGER CHECK(payload_length IS NULL OR (typeof(payload_length) = 'integer' AND payload_length >= -1)), PRIMARY KEY(key, sequence)) WITHOUT ROWID",
);

const TRIGGERS: [(&str, &str); 3] = [
    ("_uqa_mvcc_versions_INSERT_metadata", "CREATE TRIGGER _uqa_mvcc_versions_INSERT_metadata AFTER INSERT ON _uqa_mvcc_versions BEGIN INSERT INTO _uqa_mvcc_version_metadata VALUES (NEW.key, NEW.sequence, CASE WHEN NEW.value IS NULL THEN NULL WHEN typeof(NEW.value) = 'blob' THEN length(NEW.value) ELSE -1 END); END"),
    ("_uqa_mvcc_versions_UPDATE_metadata", "CREATE TRIGGER _uqa_mvcc_versions_UPDATE_metadata AFTER UPDATE ON _uqa_mvcc_versions BEGIN DELETE FROM _uqa_mvcc_version_metadata WHERE key = OLD.key AND sequence = OLD.sequence; INSERT INTO _uqa_mvcc_version_metadata VALUES (NEW.key, NEW.sequence, CASE WHEN NEW.value IS NULL THEN NULL WHEN typeof(NEW.value) = 'blob' THEN length(NEW.value) ELSE -1 END); END"),
    ("_uqa_mvcc_versions_DELETE_metadata", "CREATE TRIGGER _uqa_mvcc_versions_DELETE_metadata AFTER DELETE ON _uqa_mvcc_versions BEGIN DELETE FROM _uqa_mvcc_version_metadata WHERE key = OLD.key AND sequence = OLD.sequence; END"),
];

pub(super) fn create_triggers(connection: &Connection) -> PhysicalResult<()> {
    for (_, definition) in TRIGGERS {
        connection.execute_batch(definition)?;
    }
    Ok(())
}

pub(super) fn validate_triggers(connection: &Connection) -> PhysicalResult<()> {
    for (name, definition) in TRIGGERS {
        if schema::definition_matches(connection, name, definition)? != Some(true) {
            return Err(VersionError::InvalidEncoding(
                "missing or changed version metadata maintenance",
            )
            .into());
        }
    }
    Ok(())
}

pub(super) fn upgrade(connection: &Connection) -> PhysicalResult<()> {
    connection.execute_batch(TABLE.1)?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        connection.execute_batch(&schema::trigger(TABLE.0, action).1)?;
    }
    connection.execute_batch("INSERT INTO _uqa_mvcc_version_metadata SELECT key, sequence, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _uqa_mvcc_versions")?;
    create_triggers(connection)
}

#[cfg(test)]
pub(super) fn remove_for_predecessor(connection: &Connection) -> PhysicalResult<()> {
    for (name, _) in TRIGGERS {
        connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
    }
    connection.execute_batch("DROP TABLE _uqa_mvcc_version_metadata")?;
    Ok(())
}

#[cfg(test)]
mod tests;
