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

use super::{
    capture, graph_lookup, invalid, owners, physical, NativeMapping, NativeRecord,
    NativeRecordFamily as Family, NativeRecordNamespace,
};
use crate::mvcc::{codec, schema, write, PhysicalResult};

mod graph_lookup_upgrade;
mod occurrence_accelerators;

const LEGACY_FORMAT: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 1), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_TWO: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 2), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";
const FORMAT_THREE: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 3), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_FOUR: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 4), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_FIVE: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 5), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_SIX: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 6), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_SEVEN: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 7), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

const FORMAT_EIGHT: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 8), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

#[cfg(test)]
mod tests;

const TABLES: [(&str, &str); 4] = [
    ("_uqa_mvcc_native_format", "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 9), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49), record_namespace BLOB NOT NULL CHECK(typeof(record_namespace) = 'blob' AND length(record_namespace) = 16))"),
    ("_uqa_mvcc_native_owners", "CREATE TABLE _uqa_mvcc_native_owners (name TEXT PRIMARY KEY NOT NULL, object_id BLOB NOT NULL CHECK(typeof(object_id) = 'blob' AND length(object_id) = 16 AND object_id != zeroblob(16)), generation BLOB NOT NULL CHECK(typeof(generation) = 'blob' AND length(generation) = 16 AND generation != zeroblob(16)), catalog_owned INTEGER NOT NULL CHECK(catalog_owned IN (0, 1))) WITHOUT ROWID"),
    ("_uqa_mvcc_native_expected", "CREATE TABLE _uqa_mvcc_native_expected (family INTEGER NOT NULL, physical_key BLOB NOT NULL, old_key BLOB, new_key BLOB, new_value BLOB, PRIMARY KEY(family, physical_key), CHECK((new_key IS NULL) = (new_value IS NULL))) WITHOUT ROWID"),
    ("_uqa_mvcc_native_changes", "CREATE TABLE _uqa_mvcc_native_changes (family INTEGER NOT NULL, physical_key BLOB NOT NULL, PRIMARY KEY(family, physical_key)) WITHOUT ROWID"),
];
const OWNER_INDEX: &str =
    "CREATE UNIQUE INDEX _uqa_mvcc_native_owner_identity ON _uqa_mvcc_native_owners(object_id)";

pub(in crate::mvcc) fn present(connection: &Connection) -> PhysicalResult<bool> {
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

pub(in crate::mvcc) fn check_mapping(
    connection: &Connection,
    native: Option<NativeRecordNamespace>,
) -> PhysicalResult<()> {
    let Some(expected) = native else {
        return reject_mapped(connection);
    };
    check_mapping_version(connection, 9)?;
    if namespace(connection)? != expected {
        return Err(invalid("native record namespace changed").into());
    }
    Ok(())
}

fn namespace(connection: &Connection) -> PhysicalResult<NativeRecordNamespace> {
    let mut statement = connection
        .prepare("SELECT record_namespace FROM _uqa_mvcc_native_format WHERE singleton = 1")?;
    let mut rows = statement.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("missing native record namespace"))?;
    Ok(NativeRecordNamespace(codec::identity(codec::bytes(
        row, 0,
    )?)?))
}

fn insert_format(connection: &Connection, identity: DatabaseId) -> PhysicalResult<()> {
    connection.execute(
        "INSERT INTO _uqa_mvcc_native_format VALUES (1, 9, 49, ?1)",
        [identity.as_bytes().as_slice()],
    )?;
    Ok(())
}

fn check_mapping_version(connection: &Connection, version: u32) -> PhysicalResult<()> {
    let valid: bool = connection.query_row("SELECT (SELECT count(*) = 1 FROM _uqa_mvcc_native_format WHERE singleton = 1 AND format = ?1 AND catalog_version = 49) AND (SELECT value = '49' FROM _metadata WHERE key = 'schema_version') AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_expected) AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_changes)", [version], |row| row.get(0))?;
    if !valid {
        return Err(
            invalid("incomplete native record format or unfinished materialization").into(),
        );
    }
    Ok(())
}

fn prepare_catalog_sources(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    super::standalone_graph::import_sources(connection, control)?;
    // Bootstrap or upgrade the catalog inside the same physical transaction as conversion. A failed baseline import leaves the original file and schema intact.
    crate::Catalog::migrate_storage_in(connection)?;
    crate::Catalog::prepare_native_fts_sources(connection)?;
    let source: Option<bool> = connection
        .query_row(
            "SELECT value = '48' FROM _metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if source != Some(true) {
        return Err(invalid("native mapping requires an initialized schema 48 catalog").into());
    }
    Ok(())
}

pub(in crate::mvcc) fn initialize(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<NativeMapping> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _permit = schema::WritePermit::acquire(connection)?;
    let transaction = schema::begin(connection)?;
    let mapping = initialize_in(&transaction, control)?;
    transaction.commit()?;
    Ok(mapping)
}

pub(in crate::mvcc) fn initialize_in(
    transaction: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<NativeMapping> {
    control.cancellation().check().map_err(VersionError::from)?;
    if transaction.is_autocommit() {
        return Err(invalid("native baseline import requires an owning transaction").into());
    }
    if present(transaction)? {
        let mapping = reopen(transaction, control)?;
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(mapping);
    }
    prepare_catalog_sources(transaction, control)?;
    let identity = schema::initialize_in(transaction)?.identity;
    let header = codec::header(transaction, identity)?;
    let populated: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_heads) OR EXISTS(SELECT 1 FROM _uqa_mvcc_versions) OR EXISTS(SELECT 1 FROM _uqa_mvcc_transactions) OR EXISTS(SELECT 1 FROM _uqa_mvcc_runs)", [], |row| row.get(0))?;
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
        "_uqa_mvcc_runs",
        "_uqa_mvcc_versions",
        "_uqa_mvcc_transactions",
        "_uqa_mvcc_identifiers",
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
    transaction.execute_batch(graph_lookup::SQL)?;
    transaction.execute_batch(super::occurrence_guards::SQL)?;
    transaction.execute_batch(super::vector_guards::SQL)?;
    occurrence_accelerators::create(transaction)?;
    occurrence_accelerators::import(transaction, control)?;
    crate::Catalog::upgrade_metadata_cache_triggers(transaction)?;
    validate_layouts(transaction, 9)?;
    validate_cache_triggers(transaction, 9)?;
    owners::seed(transaction, control)?;
    super::sequences::validate_source(transaction)?;
    graph_lookup::seed(transaction, control)?;
    transaction.execute(
        "UPDATE _metadata SET value = '49' WHERE key = 'schema_version'",
        [],
    )?;
    let baseline = CommitSequence::from_u64(1);
    for family in Family::all() {
        physical::visit(transaction, family.layout(), control, |values| {
            let owner = owners::for_row(transaction, identity, family, values, control)?;
            let record = NativeRecord::encode(family, owner, values, control)?;
            owners::validate(
                transaction,
                identity,
                super::NativeRecordIdentity::new(family, owner)?,
                values,
                control,
            )?;
            write::stage_record(
                transaction,
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
    insert_format(transaction, identity)?;
    install_guards(transaction)?;
    super::standalone_graph::install_legacy_guards(transaction, control)?;
    let invalid_foreign_key = transaction
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if invalid_foreign_key {
        return Err(invalid("native source catalog violates a physical foreign key").into());
    }
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(NativeMapping {
        identity,
        namespace: NativeRecordNamespace(identity),
    })
}

fn reopen(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<NativeMapping> {
    crate::Catalog::upgrade_metadata_cache_triggers(connection)?;
    let version =
        if schema::definition_matches(connection, TABLES[0].0, LEGACY_FORMAT)? == Some(true) {
            1
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_TWO)? == Some(true) {
            2
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_THREE)? == Some(true) {
            3
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_FOUR)? == Some(true) {
            4
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_FIVE)? == Some(true) {
            5
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_SIX)? == Some(true) {
            6
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_SEVEN)? == Some(true) {
            7
        } else if schema::definition_matches(connection, TABLES[0].0, FORMAT_EIGHT)? == Some(true) {
            8
        } else {
            9
        };
    validate_format(connection, version)?;
    let initialized = schema::initialize_in(connection)?;
    let identity = initialized.identity;
    if initialized.created {
        return Err(invalid("native mapping has no record history format").into());
    }
    if codec::header(connection, identity)?.key_value_mapping {
        return Err(invalid("mixed native and KeyValue mappings").into());
    }
    check_mapping_version(connection, version)?;
    if version < 8
        && connection.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)", [], |row| {
            row.get::<_, bool>(0)
        })?
    {
        return Err(invalid("predecessor native mapping contains current record runs").into());
    }
    super::sequences::validate_source(connection)?;
    if version < 3 {
        graph_lookup_upgrade::upgrade(connection, identity, version, control)?;
    }
    if version < 4 {
        occurrence_accelerators::create(connection)?;
        for family in [Family::OccurrenceSkips, Family::OccurrenceBlockMax] {
            install_family_guards(connection, family)?;
        }
    }
    if version < 5 {
        connection.execute_batch(super::occurrence_guards::SQL)?;
        install_family_guards(connection, Family::OccurrenceGuards)?;
    }
    if version < 6 {
        connection.execute_batch(super::vector_guards::SQL)?;
        install_family_guards(connection, Family::VectorGuards)?;
        crate::Catalog::upgrade_metadata_cache_triggers(connection)?;
    }
    if version < 8 {
        super::standalone_graph::create(connection)?;
        for &(family, _) in super::standalone_graph::schema::TABLES {
            install_family_guards(connection, family)?;
        }
        for (_, sql) in graph_lookup::source_triggers(&graph_lookup::SCOPED_SOURCES) {
            connection.execute_batch(&sql)?;
        }
    }
    if version < 9 {
        connection.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        connection.execute_batch(TABLES[0].1)?;
        insert_format(connection, identity)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            connection.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        validate_format(connection, 9)?;
        check_mapping_version(connection, 9)?;
    }
    super::standalone_graph::validate_legacy_guards(connection, control)?;
    Ok(NativeMapping {
        identity,
        namespace: namespace(connection)?,
    })
}

fn install_guards(transaction: &Connection) -> PhysicalResult<()> {
    for (name, _) in TABLES {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(name, action).1)?;
        }
    }
    for family in Family::all() {
        install_family_guards(transaction, family)?;
    }
    for (_, sql) in graph_lookup::triggers() {
        transaction.execute_batch(&sql)?;
    }
    Ok(())
}

fn install_family_guards(transaction: &Connection, family: Family) -> PhysicalResult<()> {
    for action in ["INSERT", "UPDATE", "DELETE"] {
        if family != Family::TableOwners {
            transaction.execute_batch(&schema::trigger(family.layout().table, action).1)?;
        }
        transaction.execute_batch(&capture::trigger(family, action).1)?;
    }
    Ok(())
}

fn validate_format(connection: &Connection, version: u32) -> PhysicalResult<()> {
    let legacy = version == 1;
    for (name, sql) in TABLES {
        let sql = if name == TABLES[0].0 {
            match version {
                1 => LEGACY_FORMAT,
                2 => FORMAT_TWO,
                3 => FORMAT_THREE,
                4 => FORMAT_FOUR,
                5 => FORMAT_FIVE,
                6 => FORMAT_SIX,
                7 => FORMAT_SEVEN,
                8 => FORMAT_EIGHT,
                _ => sql,
            }
        } else {
            sql
        };
        require_definition(connection, name, sql)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, sql) = schema::trigger(name, action);
            require_definition(connection, &name, &sql)?;
        }
    }
    require_definition(connection, "_uqa_mvcc_native_owner_identity", OWNER_INDEX)?;
    if version >= 4 {
        for (family, sql) in occurrence_accelerators::TABLES {
            require_definition(connection, family.layout().table, sql)?;
        }
    }
    if version >= 5 {
        require_definition(
            connection,
            Family::OccurrenceGuards.layout().table,
            super::occurrence_guards::SQL,
        )?;
    }
    if version >= 6 {
        require_definition(
            connection,
            Family::VectorGuards.layout().table,
            super::vector_guards::SQL,
        )?;
    }
    if version >= 8 {
        for &(family, sql) in super::standalone_graph::schema::TABLES {
            require_definition(connection, family.layout().table, sql)?;
        }
        for (name, sql) in graph_lookup::source_triggers(&graph_lookup::SCOPED_SOURCES) {
            require_definition(connection, &name, &sql)?;
        }
    }
    for family in families(version) {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, sql) = schema::trigger(family.layout().table, action);
            require_definition(connection, &name, &sql)?;
            let (name, sql) = capture::trigger(family, action);
            require_definition(connection, &name, &sql)?;
        }
    }
    validate_cache_triggers(connection, version)?;
    if !legacy {
        require_definition(
            connection,
            Family::GraphLookups.layout().table,
            graph_lookup::SQL,
        )?;
        let sources = if version == 2 {
            &graph_lookup::SOURCES[..3]
        } else {
            &graph_lookup::SOURCES[..]
        };
        for (name, sql) in graph_lookup::source_triggers(sources) {
            require_definition(connection, &name, &sql)?;
        }
    }
    validate_layouts(connection, version)
}

fn require_definition(connection: &Connection, name: &str, sql: &str) -> PhysicalResult<()> {
    if schema::definition_matches(connection, name, sql)? != Some(true) {
        return Err(invalid("missing or changed native materialization schema or guard").into());
    }
    Ok(())
}

fn validate_cache_triggers(connection: &Connection, version: u32) -> PhysicalResult<()> {
    for family in families(version).filter(|family| {
        !family.is_standalone_graph()
            && !matches!(
                family,
                Family::CacheRevisions
                    | Family::TableOwners
                    | Family::OccurrenceGuards
                    | Family::VectorGuards
                    | Family::GraphLookups
                    | Family::GraphPathPairs
                    | Family::GraphPathIndexState
            )
    }) {
        let layout = family.layout();
        for event in ["INSERT", "DELETE", "UPDATE"] {
            let (name, sql) = crate::Catalog::cache_revision_trigger(
                layout.table,
                layout.columns.contains(&"table_name"),
                event,
            );
            require_definition(connection, &name, &sql)?;
        }
    }
    Ok(())
}

fn families(version: u32) -> impl Iterator<Item = Family> {
    Family::all().filter(move |family| match family {
        family if family.is_standalone_graph() => version >= 8,
        Family::GraphLookups => version >= 2,
        Family::OccurrenceSkips | Family::OccurrenceBlockMax => version >= 4,
        Family::OccurrenceGuards => version >= 5,
        Family::VectorGuards => version >= 6,
        _ => true,
    })
}

fn validate_layouts(connection: &Connection, version: u32) -> PhysicalResult<()> {
    for family in families(version) {
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
        if !families(version).any(|family| family.layout().table == name)
            && !TABLES.iter().any(|(table, _)| *table == name)
            && !matches!(
                name,
                "_uqa_mvcc_metadata"
                    | "_uqa_mvcc_heads"
                    | "_uqa_mvcc_runs"
                    | "_uqa_mvcc_versions"
                    | "_uqa_mvcc_transactions"
                    | "_uqa_mvcc_identifiers"
            )
        {
            return Err(invalid("unmapped native table requires an explicit record family").into());
        }
    }
    Ok(())
}
