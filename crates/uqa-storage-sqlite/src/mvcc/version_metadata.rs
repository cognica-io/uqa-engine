//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keyed predecessor metadata addresses append-ordered payload records in the same transaction.

use rusqlite::Connection;

use super::{schema, PhysicalResult, VersionError};

pub(super) const ADDRESSED_TABLE: &str = "CREATE TABLE _uqa_mvcc_version_metadata (key BLOB NOT NULL, sequence BLOB NOT NULL, version_id INTEGER NOT NULL CHECK(version_id > 0), payload_length INTEGER CHECK(payload_length IS NULL OR (typeof(payload_length) = 'integer' AND payload_length >= -1)), PRIMARY KEY(key, sequence)) WITHOUT ROWID";

pub(super) const TABLE: (&str, &str) = (
    "_uqa_mvcc_version_metadata",
    "CREATE TABLE _uqa_mvcc_version_metadata (key BLOB NOT NULL, sequence BLOB NOT NULL, version_id INTEGER NOT NULL CHECK(version_id > 0), payload_length INTEGER CHECK(payload_length IS NULL OR (typeof(payload_length) = 'integer' AND payload_length >= -1)), image_digest BLOB, PRIMARY KEY(key, sequence)) WITHOUT ROWID",
);

// The explicit INTEGER PRIMARY KEY keeps addresses stable across VACUUM. Addresses
// are private to the physical snapshot; reclaimed addresses may safely be reused.
pub(super) const VERSIONS_TABLE: (&str, &str) = (
    "_uqa_mvcc_versions",
    "CREATE TABLE _uqa_mvcc_versions (key BLOB NOT NULL CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), value BLOB CHECK(value IS NULL OR typeof(value) = 'blob'), version_id INTEGER PRIMARY KEY CHECK(version_id > 0))",
);

pub(super) const PREVIOUS_TABLE: &str = "CREATE TABLE _uqa_mvcc_version_metadata (key BLOB NOT NULL, sequence BLOB NOT NULL, payload_length INTEGER CHECK(payload_length IS NULL OR (typeof(payload_length) = 'integer' AND payload_length >= -1)), PRIMARY KEY(key, sequence)) WITHOUT ROWID";

pub(super) const PREVIOUS_VERSIONS_TABLE: &str = "CREATE TABLE _uqa_mvcc_versions (key BLOB NOT NULL CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), value BLOB CHECK(value IS NULL OR typeof(value) = 'blob'), PRIMARY KEY(key, sequence)) WITHOUT ROWID";

const PREVIOUS_TRIGGERS: [(&str, &str); 3] = [
    ("_uqa_mvcc_versions_INSERT_metadata", "CREATE TRIGGER _uqa_mvcc_versions_INSERT_metadata AFTER INSERT ON _uqa_mvcc_versions BEGIN INSERT INTO _uqa_mvcc_version_metadata VALUES (NEW.key, NEW.sequence, CASE WHEN NEW.value IS NULL THEN NULL WHEN typeof(NEW.value) = 'blob' THEN length(NEW.value) ELSE -1 END); END"),
    ("_uqa_mvcc_versions_UPDATE_metadata", "CREATE TRIGGER _uqa_mvcc_versions_UPDATE_metadata AFTER UPDATE ON _uqa_mvcc_versions BEGIN DELETE FROM _uqa_mvcc_version_metadata WHERE key = OLD.key AND sequence = OLD.sequence; INSERT INTO _uqa_mvcc_version_metadata VALUES (NEW.key, NEW.sequence, CASE WHEN NEW.value IS NULL THEN NULL WHEN typeof(NEW.value) = 'blob' THEN length(NEW.value) ELSE -1 END); END"),
    ("_uqa_mvcc_versions_DELETE_metadata", "CREATE TRIGGER _uqa_mvcc_versions_DELETE_metadata AFTER DELETE ON _uqa_mvcc_versions BEGIN DELETE FROM _uqa_mvcc_version_metadata WHERE key = OLD.key AND sequence = OLD.sequence; END"),
];

pub(super) fn create_triggers(connection: &Connection) -> PhysicalResult<()> {
    for (_, definition) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&fingerprint_trigger(definition))?;
    }
    Ok(())
}

pub(super) fn validate_triggers(
    connection: &Connection,
    addressed: bool,
    fingerprints: bool,
) -> PhysicalResult<()> {
    for (name, definition) in PREVIOUS_TRIGGERS {
        let expected = if fingerprints {
            fingerprint_trigger(definition)
        } else if addressed {
            addressed_trigger(definition)
        } else {
            definition.to_owned()
        };
        if schema::definition_matches(connection, name, &expected)? != Some(true) {
            return Err(VersionError::InvalidEncoding(
                "missing or changed version metadata maintenance",
            )
            .into());
        }
    }
    Ok(())
}

fn addressed_trigger(definition: &str) -> String {
    definition.replace("NEW.sequence, CASE", "NEW.sequence, NEW.version_id, CASE")
}

fn fingerprint_trigger(definition: &str) -> String {
    addressed_trigger(definition).replace(
        "INSERT INTO _uqa_mvcc_version_metadata VALUES",
        "INSERT INTO _uqa_mvcc_version_metadata (key, sequence, version_id, payload_length) VALUES",
    )
}

pub(super) fn add_fingerprint_column(connection: &Connection) -> PhysicalResult<()> {
    for (name, _) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
    }
    connection
        .execute_batch("ALTER TABLE _uqa_mvcc_version_metadata ADD COLUMN image_digest BLOB")?;
    create_triggers(connection)
}

pub(super) fn set_image_digest(
    connection: &Connection,
    key: &[u8],
    revision: uqa_storage::mvcc::CommitSequence,
    digest: uqa_storage::backup::ImageDigest,
) -> PhysicalResult<()> {
    let mut update = connection.prepare_cached(
        "UPDATE _uqa_mvcc_version_metadata SET image_digest = ?3 WHERE key = ?1 AND sequence = ?2 AND payload_length >= 0",
    )?;
    let changed = update.execute(rusqlite::params![
        key,
        revision.as_u64().to_be_bytes().as_slice(),
        digest.as_bytes().as_slice()
    ])?;
    if changed != 1 {
        return Err(
            VersionError::InvalidEncoding("record fingerprint has no matching version").into(),
        );
    }
    Ok(())
}

pub(super) fn retain_image_digest(
    connection: &Connection,
    key: &[u8],
    revision: uqa_storage::mvcc::CommitSequence,
    value: Option<&[u8]>,
    control: &uqa_storage::read_control::StorageReadControl,
) -> PhysicalResult<()> {
    if let Some(value) = value {
        let digest = uqa_storage::backup::RecordImageHasher::for_value(revision, value, control)?;
        set_image_digest(connection, key, revision, digest)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn install_addressed_predecessor(connection: &Connection) -> PhysicalResult<()> {
    for (name, _) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
    }
    connection.execute_batch("ALTER TABLE _uqa_mvcc_version_metadata DROP COLUMN image_digest")?;
    for (_, definition) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&addressed_trigger(definition))?;
    }
    Ok(())
}

fn remove_metadata(connection: &Connection) -> PhysicalResult<()> {
    for (name, _) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
    }
    connection.execute_batch("DROP TABLE _uqa_mvcc_version_metadata")?;
    Ok(())
}

mod upgrade;
#[cfg(test)]
pub(super) use upgrade::install_predecessor;
pub(super) use upgrade::upgrade;

#[cfg(test)]
mod tests;
