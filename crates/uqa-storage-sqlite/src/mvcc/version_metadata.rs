//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keyed predecessor metadata addresses append-ordered payload records in the same transaction.

use rusqlite::{params, CachedStatement, Connection};
use uqa_storage::{mvcc::CommitSequence, read_control::StorageReadControl};

use super::{connection_functions::FingerprintScope, schema, PhysicalResult, VersionError};

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
    create_fingerprint_triggers(connection, true)
}

fn create_fingerprint_triggers(connection: &Connection, addressed: bool) -> PhysicalResult<()> {
    for (name, definition) in PREVIOUS_TRIGGERS {
        if name == PREVIOUS_TRIGGERS[0].0 {
            connection.execute_batch(&insert_digest_trigger(addressed))?;
        } else {
            connection.execute_batch(&fingerprint_trigger(definition, addressed))?;
        }
    }
    Ok(())
}

pub(super) fn validate_triggers(connection: &Connection, format: i64) -> PhysicalResult<()> {
    for (name, definition) in PREVIOUS_TRIGGERS {
        let expected = if format >= 70 && name == PREVIOUS_TRIGGERS[0].0 {
            insert_digest_trigger(true)
        } else if format >= 69 {
            fingerprint_trigger(definition, true)
        } else if format >= 59 {
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

fn fingerprint_trigger(definition: &str, addressed: bool) -> String {
    let (definition, insert) = if addressed {
        (
            addressed_trigger(definition),
            "INSERT INTO _uqa_mvcc_version_metadata (key, sequence, version_id, payload_length) VALUES",
        )
    } else {
        (
            definition.to_owned(),
            "INSERT INTO _uqa_mvcc_version_metadata (key, sequence, payload_length) VALUES",
        )
    };
    definition.replace("INSERT INTO _uqa_mvcc_version_metadata VALUES", insert)
}

fn insert_digest_trigger(addressed: bool) -> String {
    let (column, address) = if addressed {
        ("version_id, ", "NEW.version_id, ")
    } else {
        ("", "")
    };
    format!("CREATE TRIGGER _uqa_mvcc_versions_INSERT_metadata AFTER INSERT ON _uqa_mvcc_versions BEGIN INSERT INTO _uqa_mvcc_version_metadata (key, sequence, {column}payload_length, image_digest) VALUES (NEW.key, NEW.sequence, {address}CASE WHEN NEW.value IS NULL THEN NULL WHEN typeof(NEW.value) = 'blob' THEN length(NEW.value) ELSE -1 END, __uqa_mvcc_image_digest(NEW.sequence, NEW.value)); END")
}

pub(super) fn upgrade_insert_digest(connection: &Connection) -> PhysicalResult<()> {
    connection.execute_batch("DROP TRIGGER _uqa_mvcc_versions_INSERT_metadata")?;
    connection.execute_batch(&insert_digest_trigger(true))?;
    Ok(())
}

#[cfg(test)]
pub(super) fn install_fingerprint_predecessor(connection: &Connection) -> PhysicalResult<()> {
    connection.execute_batch("DROP TRIGGER _uqa_mvcc_versions_INSERT_metadata")?;
    connection.execute_batch(&fingerprint_trigger(PREVIOUS_TRIGGERS[0].1, true))?;
    Ok(())
}

pub(super) fn add_fingerprint_column(connection: &Connection) -> PhysicalResult<()> {
    add_fingerprint_column_for_layout(connection, true)
}

fn add_fingerprint_column_for_layout(
    connection: &Connection,
    addressed: bool,
) -> PhysicalResult<()> {
    for (name, _) in PREVIOUS_TRIGGERS {
        connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
    }
    connection
        .execute_batch("ALTER TABLE _uqa_mvcc_version_metadata ADD COLUMN image_digest BLOB")?;
    create_fingerprint_triggers(connection, addressed)
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
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

pub(super) struct VersionInsert<'a> {
    statement: CachedStatement<'a>,
    fingerprints: FingerprintScope,
}

impl<'a> VersionInsert<'a> {
    pub(super) fn new(
        connection: &'a Connection,
        control: &StorageReadControl,
    ) -> PhysicalResult<Self> {
        let fingerprints = FingerprintScope::new(connection, control)?;
        let statement = connection.prepare_cached(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, ?3)",
        )?;
        Ok(Self {
            statement,
            fingerprints,
        })
    }

    pub(super) fn insert(
        &mut self,
        key: &[u8],
        sequence: CommitSequence,
        value: Option<&[u8]>,
    ) -> PhysicalResult<()> {
        let result = self.statement.execute(params![
            key,
            sequence.as_u64().to_be_bytes().as_slice(),
            value
        ]);
        self.statement.clear_bindings();
        self.fingerprints.resolve(result)
    }
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
