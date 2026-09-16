//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic conversion of the current native catalog into a guarded materialization and its MVCC baseline.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId, VersionError},
    read_control::StorageReadControl,
};

use super::{capture, invalid, owners, physical, NativeRecord, NativeRecordFamily as Family};
use crate::mvcc::{codec, schema, write, PhysicalResult};

const TABLES: [(&str, &str); 4] = [
    ("_uqa_mvcc_native_format", "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 1), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))"),
    ("_uqa_mvcc_native_owners", "CREATE TABLE _uqa_mvcc_native_owners (name TEXT PRIMARY KEY NOT NULL, object_id BLOB NOT NULL CHECK(typeof(object_id) = 'blob' AND length(object_id) = 16 AND object_id != zeroblob(16)), generation BLOB NOT NULL CHECK(typeof(generation) = 'blob' AND length(generation) = 16 AND generation != zeroblob(16)), catalog_owned INTEGER NOT NULL CHECK(catalog_owned IN (0, 1))) WITHOUT ROWID"),
    ("_uqa_mvcc_native_expected", "CREATE TABLE _uqa_mvcc_native_expected (family INTEGER NOT NULL, physical_key BLOB NOT NULL, old_key BLOB, new_key BLOB, new_value BLOB, PRIMARY KEY(family, physical_key), CHECK((new_key IS NULL) = (new_value IS NULL))) WITHOUT ROWID"),
    ("_uqa_mvcc_native_changes", "CREATE TABLE _uqa_mvcc_native_changes (family INTEGER NOT NULL, physical_key BLOB NOT NULL, PRIMARY KEY(family, physical_key)) WITHOUT ROWID"),
];
const OWNER_INDEX: &str =
    "CREATE UNIQUE INDEX _uqa_mvcc_native_owner_identity ON _uqa_mvcc_native_owners(object_id)";

fn present(connection: &Connection) -> PhysicalResult<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name GLOB '_uqa_mvcc_native_*')",
        [],
        |row| row.get(0),
    )?)
}

pub(in crate::mvcc) fn reject_mapped(connection: &Connection) -> PhysicalResult<()> {
    if present(connection)? {
        return Err(
            invalid("native records require their materializing persistence adapter").into(),
        );
    }
    Ok(())
}

pub(in crate::mvcc) fn check_mapping(connection: &Connection, native: bool) -> PhysicalResult<()> {
    if !native {
        return reject_mapped(connection);
    }
    let valid: bool = connection.query_row("SELECT (SELECT count(*) = 1 FROM _uqa_mvcc_native_format WHERE singleton = 1 AND format = 1 AND catalog_version = 49) AND (SELECT value = '49' FROM _metadata WHERE key = 'schema_version') AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_expected) AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_changes)", [], |row| row.get(0))?;
    if !valid {
        return Err(
            invalid("incomplete native record format or unfinished materialization").into(),
        );
    }
    Ok(())
}

pub(in crate::mvcc) fn initialize(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<DatabaseId> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
    if present(&transaction)? {
        validate_format(&transaction)?;
        let (identity, created) = schema::initialize_in(&transaction)?;
        if created {
            return Err(invalid("native mapping has no record history format").into());
        }
        if codec::header(&transaction, identity)?.key_value_mapping {
            return Err(invalid("mixed native and KeyValue mappings").into());
        }
        check_mapping(&transaction, true)?;
        return Ok(identity);
    }
    let source: Option<bool> = transaction
        .query_row(
            "SELECT value = '48' FROM _metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if source != Some(true) {
        return Err(invalid("native mapping requires an initialized schema 48 catalog").into());
    }
    let (identity, _) = schema::initialize_in(&transaction)?;
    let header = codec::header(&transaction, identity)?;
    let populated: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_heads) OR EXISTS(SELECT 1 FROM _uqa_mvcc_versions) OR EXISTS(SELECT 1 FROM _uqa_mvcc_transactions)", [], |row| row.get(0))?;
    if header.key_value_mapping
        || header.allocated != 0
        || header.sequence.as_u64() != 0
        || populated
    {
        return Err(
            invalid("cannot merge native catalog rows into an existing record history").into(),
        );
    }
    // Older catalogs could install their generic cache triggers on raw record tables. Internal persistence bookkeeping must never change the logical cache generations.
    for table in [
        "_uqa_mvcc_metadata",
        "_uqa_mvcc_heads",
        "_uqa_mvcc_versions",
        "_uqa_mvcc_transactions",
    ] {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS uqa_cache_{table}_{action}"
            ))?;
        }
    }
    for (_, sql) in TABLES {
        transaction.execute_batch(sql)?;
    }
    transaction.execute_batch(OWNER_INDEX)?;
    validate_layouts(&transaction)?;
    owners::seed(&transaction, control)?;
    transaction.execute(
        "UPDATE _metadata SET value = '49' WHERE key = 'schema_version'",
        [],
    )?;
    let baseline = CommitSequence::from_u64(1);
    for family in Family::all() {
        physical::visit(&transaction, family.layout(), control, |values| {
            let owner = owners::for_row(&transaction, identity, family, values, control)?;
            let record = NativeRecord::encode(family, owner, values, control)?;
            owners::validate(
                &transaction,
                identity,
                super::NativeRecordIdentity::new(family, owner)?,
                values,
                control,
            )?;
            write::stage_record(
                &transaction,
                record.key(),
                Some(record.row()),
                baseline,
                control,
            )
        })?;
    }
    transaction.execute(
        "UPDATE _uqa_mvcc_metadata SET sequence = ?1 WHERE singleton = 1",
        params![baseline.as_u64().to_be_bytes().as_slice()],
    )?;
    transaction.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1, 1, 49)", [])?;
    install_guards(&transaction)?;
    let invalid_foreign_key = transaction
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if invalid_foreign_key {
        return Err(invalid("native source catalog violates a physical foreign key").into());
    }
    control.cancellation().check().map_err(VersionError::from)?;
    transaction.commit()?;
    Ok(identity)
}

fn install_guards(transaction: &Connection) -> PhysicalResult<()> {
    for (name, _) in TABLES {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(name, action).1)?;
        }
    }
    for family in Family::all() {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            if family != Family::TableOwners {
                transaction.execute_batch(&schema::trigger(family.layout().table, action).1)?;
            }
            transaction.execute_batch(&capture::trigger(family, action).1)?;
        }
    }
    Ok(())
}

fn validate_format(connection: &Connection) -> PhysicalResult<()> {
    for (name, sql) in TABLES {
        require_definition(connection, name, sql)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, sql) = schema::trigger(name, action);
            require_definition(connection, &name, &sql)?;
        }
    }
    require_definition(connection, "_uqa_mvcc_native_owner_identity", OWNER_INDEX)?;
    for family in Family::all() {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, sql) = schema::trigger(family.layout().table, action);
            require_definition(connection, &name, &sql)?;
            let (name, sql) = capture::trigger(family, action);
            require_definition(connection, &name, &sql)?;
        }
    }
    validate_layouts(connection)
}

fn require_definition(connection: &Connection, name: &str, sql: &str) -> PhysicalResult<()> {
    if schema::definition_matches(connection, name, sql)? != Some(true) {
        return Err(invalid("missing or changed native materialization schema or guard").into());
    }
    Ok(())
}

fn validate_layouts(connection: &Connection) -> PhysicalResult<()> {
    for family in Family::all() {
        let layout = family.layout();
        let mut statement = connection.prepare(&format!("PRAGMA table_info({})", layout.table))?;
        let mut rows = statement.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            if count >= layout.columns.len() {
                return Err(invalid("unexpected native materialization column").into());
            }
            let name = row
                .get_ref(1)?
                .as_str()
                .map_err(|_| invalid("native column name is not UTF-8"))?;
            let kind = row
                .get_ref(2)?
                .as_str()
                .map_err(|_| invalid("native column type is not UTF-8"))?;
            let not_null: bool = row.get(3)?;
            let primary = usize::from(row.get::<_, u16>(5)?);
            let expected_key = layout
                .primary_key
                .iter()
                .position(|&column| column == count)
                .map_or(0, |slot| slot + 1);
            if name != layout.columns[count]
                || kind != layout.column_types[count].declaration()
                || not_null == layout.nullable[count]
                || primary != expected_key
            {
                return Err(invalid(
                    "native materialization columns do not match the record layout",
                )
                .into());
            }
            count += 1;
        }
        if count != layout.columns.len() {
            return Err(invalid("missing native materialization columns").into());
        }
    }
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT GLOB 'sqlite_*'",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| invalid("native table name is not UTF-8"))?;
        if !Family::all().any(|family| family.layout().table == name)
            && !TABLES.iter().any(|(table, _)| *table == name)
            && !matches!(
                name,
                "_uqa_mvcc_metadata"
                    | "_uqa_mvcc_heads"
                    | "_uqa_mvcc_versions"
                    | "_uqa_mvcc_transactions"
            )
        {
            return Err(invalid("unmapped native table requires an explicit record family").into());
        }
    }
    Ok(())
}
