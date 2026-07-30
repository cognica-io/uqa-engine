//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog: schema versioning, migrations, and persisted table metadata.
//!
//! The catalog owns a single `_metadata` table that records the schema version
//! plus a `_tables` table holding per-table analyzer config and the lists
//! of FTS / vector fields registered on each table. Concrete data tables
//! (`_documents`, `_document_blobs`, `_postings`, `_vectors`, ...) are created
//! by catalog migrations before their respective stores are exposed.

use rusqlite::{params, OptionalExtension};

use crate::backend::{StorageBackendError, StorageBackendResult};
use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    GraphSnapshot, RelationIdentity, RelationKind, SequenceRow, TableSchema, VectorFieldSchema,
    ViewRow,
};
use crate::sqlite::connection::{ManagedConnection, Result, SQLiteError};

use super::catalog_lifecycle::{
    columns_json_references, delete_table_rows_if_exists, drop_fts_aux_tables_for_field,
    drop_fts_aux_tables_for_table, quote_sql_identifier, rename_btree_field_rows_or_keep_existing,
    rename_field_rows_or_keep_existing, rename_fts_aux_tables_for_field, renamed_columns_json,
    table_exists, update_btree_table_name_rows_if_exists, update_table_name_rows_if_exists,
};

/// Bump this every time a migration is added.
pub const CURRENT_SCHEMA_VERSION: u32 = 18;

const LEGACY_VIEWS_METADATA_KEY: &str = "sql_views_json";
const LEGACY_SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";

pub struct Catalog {
    conn: ManagedConnection,
    fts_storage_was_reset: bool,
}

fn encode_catalog_id(kind: &str, id: u64) -> Result<i64> {
    i64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} id {id} exceeds the SQLite INTEGER range"))
    })
}

fn decode_catalog_id(kind: &str, id: i64) -> Result<u64> {
    u64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt catalog: negative {kind} id {id}"))
    })
}

#[derive(serde::Deserialize)]
struct LegacySequenceState {
    start: i64,
    increment: i64,
    current: i64,
}

fn migration_relation(value: &str) -> Result<RelationIdentity> {
    RelationIdentity::from_legacy_name(value).map_err(SQLiteError::StorageBackend)
}

fn register_migration_relation(
    seen: &mut std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>,
    relation: &RelationIdentity,
    kind: RelationKind,
    source: String,
) -> Result<()> {
    if let Some((existing_kind, existing_source)) = seen.get(relation) {
        return Err(SQLiteError::StorageBackend(format!(
            "relation namespace migration collision for `{}`: {} `{}` and {} `{}`",
            relation.qualified_name(),
            existing_kind.as_str(),
            existing_source,
            kind.as_str(),
            source
        )));
    }
    seen.insert(relation.clone(), (kind, source));
    Ok(())
}

type SqliteSeenRelations = std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>;

struct SqliteTableMigration {
    old_name: String,
    relation: RelationIdentity,
    analyzer: String,
    fts: String,
    vectors: String,
    columns: Option<String>,
    constraints: String,
}

struct SqliteSequenceMigration {
    relation: RelationIdentity,
    start: i64,
    increment: i64,
    current: i64,
}

struct SqliteForeignMigration {
    relation: RelationIdentity,
    server: String,
    columns: String,
    options: String,
}

struct SqliteRelationMigrations {
    seen: SqliteSeenRelations,
    tables: Vec<SqliteTableMigration>,
    sequences: Vec<SqliteSequenceMigration>,
    foreign_tables: Vec<SqliteForeignMigration>,
    views: Vec<(RelationIdentity, String)>,
}

fn load_legacy_tables(tx: &rusqlite::Transaction<'_>) -> Result<Vec<SqliteTableMigration>> {
    let mut stmt = tx.prepare(
        "SELECT name, analyzer, fts_fields, vector_fields, columns, constraints
           FROM _tables ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut tables = Vec::new();
    for row in rows {
        let (old_name, analyzer, fts, vectors, columns, constraints) = row?;
        tables.push(SqliteTableMigration {
            relation: migration_relation(&old_name)?,
            old_name,
            analyzer,
            fts,
            vectors,
            columns,
            constraints,
        });
    }
    Ok(tables)
}

fn load_legacy_sequences(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(String, SqliteSequenceMigration)>> {
    let mut stmt =
        tx.prepare("SELECT name, start, increment, current FROM _sequences ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut sequences = Vec::new();
    for row in rows {
        let (name, start, increment, current) = row?;
        sequences.push((
            name.clone(),
            SqliteSequenceMigration {
                relation: migration_relation(&name)?,
                start,
                increment,
                current,
            },
        ));
    }
    Ok(sequences)
}

fn load_legacy_foreign_tables(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(String, SqliteForeignMigration)>> {
    let mut stmt = tx.prepare(
        "SELECT name, server_name, columns_json, options
           FROM _foreign_tables ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut foreign_tables = Vec::new();
    for row in rows {
        let (name, server, columns, options) = row?;
        foreign_tables.push((
            name.clone(),
            SqliteForeignMigration {
                relation: migration_relation(&name)?,
                server,
                columns,
                options,
            },
        ));
    }
    Ok(foreign_tables)
}

fn load_legacy_metadata<T>(tx: &rusqlite::Transaction<'_>, key: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    tx.query_row(
        "SELECT value FROM _metadata WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|json| serde_json::from_str::<T>(&json))
    .transpose()
    .map(Option::unwrap_or_default)
    .map_err(SQLiteError::from)
}

fn collect_sqlite_relation_migrations(
    tx: &rusqlite::Transaction<'_>,
) -> Result<SqliteRelationMigrations> {
    let tables = load_legacy_tables(tx)?;
    let mut sequences = load_legacy_sequences(tx)?;
    let foreign_tables = load_legacy_foreign_tables(tx)?;
    let legacy_views = load_legacy_metadata::<serde_json::Map<String, serde_json::Value>>(
        tx,
        LEGACY_VIEWS_METADATA_KEY,
    )?;
    let legacy_sequences = load_legacy_metadata::<
        std::collections::BTreeMap<String, LegacySequenceState>,
    >(tx, LEGACY_SEQUENCES_METADATA_KEY)?;
    let mut seen = SqliteSeenRelations::new();
    for table in &tables {
        register_migration_relation(
            &mut seen,
            &table.relation,
            RelationKind::Table,
            table.old_name.clone(),
        )?;
    }
    for (source, sequence) in &sequences {
        register_migration_relation(
            &mut seen,
            &sequence.relation,
            RelationKind::Sequence,
            source.clone(),
        )?;
    }
    for (source, foreign) in &foreign_tables {
        register_migration_relation(
            &mut seen,
            &foreign.relation,
            RelationKind::ForeignTable,
            source.clone(),
        )?;
    }
    let mut views = Vec::new();
    for (name, definition) in legacy_views {
        let relation = migration_relation(&name)?;
        register_migration_relation(
            &mut seen,
            &relation,
            RelationKind::View,
            format!("legacy metadata `{name}`"),
        )?;
        views.push((relation, serde_json::to_string(&definition)?));
    }
    for (name, state) in legacy_sequences {
        let relation = migration_relation(&name)?;
        register_migration_relation(
            &mut seen,
            &relation,
            RelationKind::Sequence,
            format!("legacy metadata `{name}`"),
        )?;
        sequences.push((
            name,
            SqliteSequenceMigration {
                relation,
                start: state.start,
                increment: state.increment,
                current: state.current,
            },
        ));
    }
    Ok(SqliteRelationMigrations {
        seen,
        tables,
        sequences: sequences.into_iter().map(|(_, row)| row).collect(),
        foreign_tables: foreign_tables.into_iter().map(|(_, row)| row).collect(),
        views,
    })
}

fn create_structural_relation_tables(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE _relations (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL CHECK (
                kind IN ('table', 'view', 'sequence', 'foreign_table')
            ),
            PRIMARY KEY (schema_name, relation_name),
            UNIQUE (schema_name, relation_name, kind),
            FOREIGN KEY (schema_name) REFERENCES _schemas(name) ON DELETE RESTRICT
        );
        CREATE TABLE _tables_v17 (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL DEFAULT 'table' CHECK (kind = 'table'),
            analyzer      TEXT NOT NULL,
            fts_fields    TEXT NOT NULL,
            vector_fields TEXT NOT NULL,
            columns       TEXT,
            constraints   TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _sequences_v17 (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL DEFAULT 'sequence' CHECK (kind = 'sequence'),
            start         INTEGER NOT NULL,
            increment     INTEGER NOT NULL,
            current       INTEGER NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _foreign_tables_v17 (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL DEFAULT 'foreign_table' CHECK (kind = 'foreign_table'),
            server_name   TEXT NOT NULL,
            columns_json  TEXT NOT NULL,
            options       TEXT NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _views (
            schema_name     TEXT NOT NULL,
            relation_name   TEXT NOT NULL,
            kind            TEXT NOT NULL DEFAULT 'view' CHECK (kind = 'view'),
            definition_json TEXT NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );",
    )?;
    Ok(())
}

fn insert_relation_parents(
    tx: &rusqlite::Transaction<'_>,
    seen: &SqliteSeenRelations,
) -> Result<()> {
    for (relation, (kind, _)) in seen {
        tx.execute(
            "INSERT OR IGNORE INTO _schemas(name) VALUES (?1)",
            params![relation.schema],
        )?;
        tx.execute(
            "INSERT INTO _relations(schema_name, relation_name, kind) VALUES (?1, ?2, ?3)",
            params![relation.schema, relation.name, kind.as_str()],
        )?;
    }
    Ok(())
}

fn migrate_sqlite_table_name(
    tx: &rusqlite::Transaction<'_>,
    table: &SqliteTableMigration,
) -> Result<()> {
    let canonical = table.relation.qualified_name();
    if table.old_name == canonical {
        return Ok(());
    }
    for owner_table in [
        "_documents",
        "_document_blobs",
        "_postings",
        "_doc_lengths",
        "_field_stats",
        "_vectors",
        "_ivf_indexes",
        "_ivf_centroids",
        "_ivf_assignments",
        "_column_stats",
        "_table_field_analyzers",
        "_catalog_indexes",
    ] {
        if table_exists(tx, owner_table)? {
            let target_rows: i64 = tx.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE table_name = ?1",
                    quote_sql_identifier(owner_table)
                ),
                params![canonical],
                |row| row.get(0),
            )?;
            if target_rows != 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation namespace migration for `{canonical}` would overwrite existing rows in `{owner_table}`"
                )));
            }
        }
        update_table_name_rows_if_exists(tx, owner_table, &table.old_name, &canonical)?;
    }
    // `_btree_index_entries` is a child of `_btree_indexes`. Check both
    // destinations before moving either, then update parent-first. The helper
    // also moves children explicitly when an externally supplied SQLite
    // connection has foreign-key enforcement disabled.
    for owner_table in ["_btree_indexes", "_btree_index_entries"] {
        if table_exists(tx, owner_table)? {
            let target_rows: i64 = tx.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE table_name = ?1",
                    quote_sql_identifier(owner_table)
                ),
                params![canonical],
                |row| row.get(0),
            )?;
            if target_rows != 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation namespace migration for `{canonical}` would overwrite existing rows in `{owner_table}`"
                )));
            }
        }
    }
    update_btree_table_name_rows_if_exists(tx, &table.old_name, &canonical)?;
    drop_fts_aux_tables_for_table(tx, &table.old_name)
}

fn insert_table_migration(
    tx: &rusqlite::Transaction<'_>,
    table: &SqliteTableMigration,
) -> Result<()> {
    migrate_sqlite_table_name(tx, table)?;
    tx.execute(
        "INSERT INTO _tables_v17
            (schema_name, relation_name, kind, analyzer, fts_fields,
             vector_fields, columns, constraints)
         VALUES (?1, ?2, 'table', ?3, ?4, ?5, ?6, ?7)",
        params![
            table.relation.schema,
            table.relation.name,
            table.analyzer,
            table.fts,
            table.vectors,
            table.columns,
            table.constraints
        ],
    )?;
    Ok(())
}

fn insert_relation_children(
    tx: &rusqlite::Transaction<'_>,
    migrations: &SqliteRelationMigrations,
) -> Result<()> {
    for table in &migrations.tables {
        insert_table_migration(tx, table)?;
    }
    for sequence in &migrations.sequences {
        tx.execute(
            "INSERT INTO _sequences_v17
                (schema_name, relation_name, kind, start, increment, current)
             VALUES (?1, ?2, 'sequence', ?3, ?4, ?5)",
            params![
                sequence.relation.schema,
                sequence.relation.name,
                sequence.start,
                sequence.increment,
                sequence.current
            ],
        )?;
    }
    for foreign in &migrations.foreign_tables {
        tx.execute(
            "INSERT INTO _foreign_tables_v17
                (schema_name, relation_name, kind, server_name, columns_json, options)
             VALUES (?1, ?2, 'foreign_table', ?3, ?4, ?5)",
            params![
                foreign.relation.schema,
                foreign.relation.name,
                foreign.server,
                foreign.columns,
                foreign.options
            ],
        )?;
    }
    for (relation, definition_json) in &migrations.views {
        tx.execute(
            "INSERT INTO _views
                (schema_name, relation_name, kind, definition_json)
             VALUES (?1, ?2, 'view', ?3)",
            params![relation.schema, relation.name, definition_json],
        )?;
    }
    Ok(())
}

fn finish_sqlite_relation_migration(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP TABLE _tables;
         ALTER TABLE _tables_v17 RENAME TO _tables;
         DROP TABLE _sequences;
         ALTER TABLE _sequences_v17 RENAME TO _sequences;
         DROP TABLE _foreign_tables;
         ALTER TABLE _foreign_tables_v17 RENAME TO _foreign_tables;",
    )?;
    for key in [LEGACY_VIEWS_METADATA_KEY, LEGACY_SEQUENCES_METADATA_KEY] {
        tx.execute(
            "INSERT OR REPLACE INTO _metadata(key, value) VALUES (?1, '{}')",
            params![key],
        )?;
    }
    Ok(())
}

impl Catalog {
    /// Open (or create) the catalog and run any pending migrations.
    pub fn open(conn: ManagedConnection) -> Result<Self> {
        let mut cat = Self {
            conn,
            fts_storage_was_reset: false,
        };
        cat.fts_storage_was_reset = cat.run_migrations()?;
        Ok(cat)
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    fn run_migrations(&self) -> Result<bool> {
        self.conn.with_mut(|conn| {
            // Older catalogs (pre-v7) used the table name `_meta`. v7
            // renames it to `_metadata`; promote the legacy table before
            // any migration query touches it.
            let legacy_meta_only: bool = conn
                .query_row(
                    "SELECT \
                        (SELECT COUNT(*) FROM sqlite_master \
                          WHERE type='table' AND name='_meta') > 0 \
                     AND (SELECT COUNT(*) FROM sqlite_master \
                            WHERE type='table' AND name='_metadata') = 0",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_some_and(|n| n != 0);
            if legacy_meta_only {
                conn.execute("ALTER TABLE _meta RENAME TO _metadata", [])?;
            }
            conn.execute(
                "CREATE TABLE IF NOT EXISTS _metadata (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )?;
            let current = conn
                .query_row(
                    "SELECT value FROM _metadata WHERE key = 'schema_version'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let current = match current {
                Some(version) => version
                    .parse::<u32>()
                    .map_err(|_| SQLiteError::InvalidSchemaVersion(version))?,
                None => 0,
            };
            if current > CURRENT_SCHEMA_VERSION {
                return Err(SQLiteError::UnsupportedSchemaVersion {
                    found: current,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }

            for (version, sql) in MIGRATIONS {
                if *version > current {
                    let schema_change_already_present = *version == 16
                        && Self::table_columns(conn, "_tables")?
                            .is_some_and(|columns| columns.contains_key("constraints"));
                    let relation_namespace_already_present = *version == 17
                        && Self::table_columns(conn, "_tables")?
                            .is_some_and(|columns| columns.contains_key("schema_name"))
                        && Self::table_columns(conn, "_relations")?.is_some()
                        && Self::table_columns(conn, "_views")?.is_some();
                    let sequence_called_already_present = *version == 18
                        && Self::table_columns(conn, "_sequences")?
                            .is_some_and(|columns| columns.contains_key("called"));
                    let tx = conn.transaction()?;
                    if *version == 17 && !relation_namespace_already_present {
                        Self::migrate_relation_namespace_v17(&tx)?;
                    } else if !schema_change_already_present && !sequence_called_already_present {
                        tx.execute_batch(sql)?;
                    }
                    tx.execute(
                        "INSERT OR REPLACE INTO _metadata (key, value) \
                         VALUES ('schema_version', ?1)",
                        params![version.to_string()],
                    )?;
                    tx.commit()?;
                }
            }
            Self::ensure_column_stats_shape(conn)?;
            let fts_storage_was_reset = Self::ensure_fts_storage_shape(conn)?;
            Ok(fts_storage_was_reset)
        })
    }

    fn migrate_relation_namespace_v17(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let migrations = collect_sqlite_relation_migrations(tx)?;
        create_structural_relation_tables(tx)?;
        insert_relation_parents(tx, &migrations.seen)?;
        insert_relation_children(tx, &migrations)?;
        finish_sqlite_relation_migration(tx)
    }

    fn claim_relation(
        conn: &rusqlite::Connection,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let schema_exists = conn
            .query_row(
                "SELECT 1 FROM _schemas WHERE name = ?1",
                params![relation.schema],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !schema_exists {
            return Err(SQLiteError::StorageBackend(format!(
                "schema `{}` does not exist for relation `{}`",
                relation.schema,
                relation.qualified_name()
            )));
        }
        let existing = conn
            .query_row(
                "SELECT kind FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(existing) if existing == kind.as_str() => Ok(()),
            Some(existing) => Err(SQLiteError::StorageBackend(format!(
                "relation `{}` already exists as {existing}",
                relation.qualified_name()
            ))),
            None => {
                conn.execute(
                    "INSERT INTO _relations(schema_name, relation_name, kind)
                     VALUES (?1, ?2, ?3)",
                    params![relation.schema, relation.name, kind.as_str()],
                )?;
                Ok(())
            }
        }
    }

    fn release_relation(
        conn: &rusqlite::Connection,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let existing = conn
            .query_row(
                "SELECT kind FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != kind.as_str() {
                return Err(SQLiteError::StorageBackend(format!(
                    "catalog relation `{}` is {existing}, not {}",
                    relation.qualified_name(),
                    kind.as_str()
                )));
            }
            conn.execute(
                "DELETE FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
        }
        Ok(())
    }

    fn ensure_fts_storage_shape(conn: &rusqlite::Connection) -> Result<bool> {
        let doc_lengths = Self::table_columns(conn, "_doc_lengths")?;
        let postings = Self::table_columns(conn, "_postings")?;
        let doc_lengths_ok = doc_lengths
            .as_ref()
            .is_some_and(|cols| cols.contains_key("field") && cols.contains_key("length"));
        let postings_ok = postings.as_ref().is_some_and(|cols| {
            cols.get("positions")
                .is_some_and(|ty| ty.eq_ignore_ascii_case("BLOB"))
        });
        if doc_lengths_ok && postings_ok {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS _field_stats (
                    table_name   TEXT NOT NULL,
                    field        TEXT NOT NULL,
                    total_length INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (table_name, field)
                );",
            )?;
            return Ok(false);
        }

        conn.execute_batch(
            "
            DROP TABLE IF EXISTS _postings;
            DROP TABLE IF EXISTS _doc_lengths;
            DROP TABLE IF EXISTS _field_stats;

            CREATE TABLE IF NOT EXISTS _postings (
                table_name TEXT NOT NULL,
                field      TEXT NOT NULL,
                term       TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                positions  BLOB NOT NULL,
                PRIMARY KEY (table_name, field, term, doc_id)
            );
            CREATE INDEX IF NOT EXISTS _postings_doc_idx
                ON _postings (table_name, doc_id);

            CREATE TABLE IF NOT EXISTS _doc_lengths (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                field      TEXT NOT NULL,
                length     INTEGER NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            );

            CREATE TABLE IF NOT EXISTS _field_stats (
                table_name   TEXT NOT NULL,
                field        TEXT NOT NULL,
                total_length INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (table_name, field)
            );
            ",
        )?;
        Ok(true)
    }

    fn table_columns(
        conn: &rusqlite::Connection,
        table_name: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>> {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            params![table_name],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if !exists {
            return Ok(None);
        }
        let mut stmt = conn.prepare(&format!(
            "PRAGMA table_info({})",
            quote_sql_identifier(table_name)
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (name, ty) = row?;
            out.insert(name, ty);
        }
        Ok(Some(out))
    }

    fn ensure_column_stats_shape(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _column_stats (
                table_name      TEXT NOT NULL,
                column_name     TEXT NOT NULL,
                distinct_count  INTEGER NOT NULL,
                null_count      INTEGER NOT NULL,
                min_value       TEXT,
                max_value       TEXT,
                row_count       INTEGER NOT NULL,
                histogram       TEXT NOT NULL DEFAULT '[]',
                mcv_values      TEXT NOT NULL DEFAULT '[]',
                mcv_frequencies TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (table_name, column_name)
            );",
        )?;
        let mut stmt = conn.prepare("PRAGMA table_info(_column_stats)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        let mut cols = std::collections::BTreeSet::new();
        for row in rows {
            cols.insert(row?);
        }
        for col in ["histogram", "mcv_values", "mcv_frequencies"] {
            if !cols.contains(col) {
                conn.execute(
                    &format!(
                        "ALTER TABLE _column_stats ADD COLUMN {col} TEXT NOT NULL DEFAULT '[]'"
                    ),
                    [],
                )?;
            }
        }
        Ok(())
    }

    /// Store an arbitrary key/value pair in the `_metadata` table.
    /// Mirrors the canonical UQA implementation's `Catalog.set_metadata`.
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        })
    }

    /// Read a key/value pair from the `_metadata` table.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            let v: Option<String> = c
                .query_row(
                    "SELECT value FROM _metadata WHERE key = ?1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(v)
        })
    }

    pub fn save_schema(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _schemas (name) VALUES (?1)",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn drop_schema(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            let relation_count: i64 = c.query_row(
                "SELECT COUNT(*) FROM _relations WHERE schema_name = ?1",
                params![name],
                |row| row.get(0),
            )?;
            if relation_count != 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "schema `{name}` still owns catalog relations"
                )));
            }
            c.execute("DELETE FROM _schemas WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_schemas(&self) -> Result<Vec<String>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM _schemas ORDER BY name")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn save_table(&self, schema: &TableSchema) -> Result<()> {
        let analyzer = schema.analyzer_json.clone();
        let fts = serde_json::to_string(&schema.fts_fields)?;
        let vectors = serde_json::to_string(&schema.vector_fields)?;
        let columns = schema.columns_json.clone();
        let constraints = schema.constraints_json.clone();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, &schema.relation, RelationKind::Table)?;
            tx.execute(
                "INSERT OR REPLACE INTO _tables
                    (schema_name, relation_name, kind, analyzer, fts_fields,
                     vector_fields, columns, constraints)
                 VALUES (?1, ?2, 'table', ?3, ?4, ?5, ?6, ?7)",
                params![
                    schema.relation.schema,
                    schema.relation.name,
                    analyzer,
                    fts,
                    vectors,
                    columns,
                    constraints
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_tables(&self) -> Result<Vec<TableSchema>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT schema_name, relation_name, analyzer, fts_fields,
                        vector_fields, columns, constraints
                   FROM _tables ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (
                    schema_name,
                    relation_name,
                    analyzer_json,
                    fts_str,
                    vec_str,
                    cols_opt,
                    constraints_json,
                ) = row?;
                let fts_fields: Vec<String> = serde_json::from_str(&fts_str)?;
                let vector_fields: Vec<VectorFieldSchema> = serde_json::from_str(&vec_str)?;
                out.push(TableSchema {
                    relation: RelationIdentity::new(schema_name, relation_name),
                    analyzer_json,
                    fts_fields,
                    vector_fields,
                    columns_json: cols_opt.unwrap_or_default(),
                    constraints_json,
                });
            }
            Ok(out)
        })
    }

    pub fn drop_table(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _tables WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
            Self::release_relation(&tx, &relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Wipe the rows owned by `table` from the per-table data tables
    /// (`_documents`, `_postings`, `_doc_lengths`, `_field_stats`,
    /// `_vectors`, IVF metadata). Run after [`Catalog::drop_table`]
    /// when the engine drops the table from its in-memory registry as
    /// well.
    pub fn purge_table_data(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            for storage_name in &storage_names {
                for table in [
                    "_documents",
                    "_document_blobs",
                    "_postings",
                    "_doc_lengths",
                    "_field_stats",
                    "_vectors",
                    "_ivf_indexes",
                    "_ivf_centroids",
                    "_ivf_assignments",
                    "_column_stats",
                    "_btree_index_entries",
                    "_btree_indexes",
                ] {
                    delete_table_rows_if_exists(&tx, table, storage_name)?;
                }
                drop_fts_aux_tables_for_table(&tx, storage_name)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_table_and_data(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _tables WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
            for storage_name in &storage_names {
                for table in [
                    "_documents",
                    "_document_blobs",
                    "_postings",
                    "_doc_lengths",
                    "_field_stats",
                    "_vectors",
                    "_ivf_indexes",
                    "_ivf_centroids",
                    "_ivf_assignments",
                    "_column_stats",
                    "_btree_index_entries",
                    "_btree_indexes",
                ] {
                    delete_table_rows_if_exists(&tx, table, storage_name)?;
                }
                tx.execute(
                    "DELETE FROM _table_field_analyzers WHERE table_name = ?1",
                    params![storage_name],
                )?;
                tx.execute(
                    "DELETE FROM _catalog_indexes WHERE table_name = ?1",
                    params![storage_name],
                )?;
                drop_fts_aux_tables_for_table(&tx, storage_name)?;
            }
            Self::release_relation(&tx, &relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rename_table_data(&self, from: &str, to: &str) -> Result<()> {
        let from_relation = migration_relation(from)?;
        let to_relation = migration_relation(to)?;
        if from_relation == to_relation {
            return Ok(());
        }
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, &to_relation, RelationKind::Table)?;
            let updated = tx.execute(
                "UPDATE _tables
                    SET schema_name = ?3, relation_name = ?4
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    from_relation.schema,
                    from_relation.name,
                    to_relation.schema,
                    to_relation.name
                ],
            )?;
            if updated == 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "table `{from}` does not exist"
                )));
            }
            for table in [
                "_documents",
                "_document_blobs",
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_column_stats",
                "_table_field_analyzers",
                "_catalog_indexes",
            ] {
                update_table_name_rows_if_exists(&tx, table, from, to)?;
            }
            update_btree_table_name_rows_if_exists(&tx, from, to)?;
            drop_fts_aux_tables_for_table(&tx, from)?;
            Self::release_relation(&tx, &from_relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_column_data(&self, table_name: &str, column_name: &str) -> Result<()> {
        let indexes = self.catalog_indexes_referencing_column(table_name, column_name)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            if table_exists(&tx, "_document_blobs")? {
                tx.execute(
                    "DELETE FROM _document_blobs WHERE table_name = ?1 AND field_name = ?2",
                    params![table_name, column_name],
                )?;
            }
            for table in [
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_btree_index_entries",
                "_btree_indexes",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE table_name = ?1 AND field = ?2"),
                    params![table_name, column_name],
                )?;
            }
            tx.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1 AND column_name = ?2",
                params![table_name, column_name],
            )?;
            tx.execute(
                "DELETE FROM _table_field_analyzers WHERE table_name = ?1 AND field = ?2",
                params![table_name, column_name],
            )?;
            for index_name in indexes {
                tx.execute(
                    "DELETE FROM _catalog_indexes WHERE name = ?1",
                    params![index_name],
                )?;
            }
            drop_fts_aux_tables_for_field(&tx, table_name, column_name)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rename_column_data(&self, table_name: &str, from: &str, to: &str) -> Result<()> {
        let index_updates = self.catalog_index_column_renames(table_name, from, to)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            rename_field_rows_or_keep_existing(
                &tx,
                "_document_blobs",
                "field_name",
                table_name,
                from,
                to,
            )?;
            for table in [
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
            ] {
                rename_field_rows_or_keep_existing(&tx, table, "field", table_name, from, to)?;
            }
            rename_btree_field_rows_or_keep_existing(&tx, table_name, from, to)?;
            rename_field_rows_or_keep_existing(
                &tx,
                "_column_stats",
                "column_name",
                table_name,
                from,
                to,
            )?;
            rename_field_rows_or_keep_existing(
                &tx,
                "_table_field_analyzers",
                "field",
                table_name,
                from,
                to,
            )?;
            for (index_name, columns_json) in index_updates {
                tx.execute(
                    "UPDATE _catalog_indexes
                        SET columns = ?2
                      WHERE name = ?1",
                    params![index_name, columns_json],
                )?;
            }
            rename_fts_aux_tables_for_field(&tx, table_name, from, to)?;
            tx.commit()?;
            Ok(())
        })
    }

    fn catalog_indexes_referencing_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name
                && columns_json_references(&row.columns_json, column_name)?
            {
                out.push(row.name);
            }
        }
        Ok(out)
    }

    fn catalog_index_column_renames(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = renamed_columns_json(&row.columns_json, from, to)? {
                out.push((row.name, columns_json));
            }
        }
        Ok(out)
    }

    pub fn save_model(&self, name: &str, json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _models (name, body) VALUES (?1, ?2)",
                params![name, json],
            )?;
            Ok(())
        })
    }

    pub fn load_models(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, body FROM _models ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn load_model(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT body FROM _models WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    pub fn drop_model(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _models WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    /// Persist Bayesian calibration parameters for a named signal.
    /// Matches UQA behavior for `Catalog.save_scoring_params`.
    pub fn save_scoring_params(&self, name: &str, params_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _scoring_params (name, params) VALUES (?1, ?2)",
                params![name, params_json],
            )?;
            Ok(())
        })
    }

    /// Load persisted scoring parameters for a single signal.
    pub fn load_scoring_params(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT params FROM _scoring_params WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    /// Load every persisted `(name, params_json)` pair sorted by name.
    pub fn load_all_scoring_params(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, params FROM _scoring_params ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Delete persisted scoring parameters for a single signal.
    pub fn drop_scoring_params(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _scoring_params WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn create_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let exists = tx
                .query_row(
                    "SELECT 1 FROM _sequences
                      WHERE schema_name = ?1 AND relation_name = ?2",
                    params![sequence.relation.schema, sequence.relation.name],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exists {
                return Ok(false);
            }
            Self::claim_relation(&tx, &sequence.relation, RelationKind::Sequence)?;
            tx.execute(
                "INSERT INTO _sequences
                    (schema_name, relation_name, kind, start, increment, current, called)
                 VALUES (?1, ?2, 'sequence', ?3, ?4, ?5, ?6)",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn replace_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with(|connection| {
            Ok(connection.execute(
                "UPDATE _sequences
                    SET start = ?3, increment = ?4, current = ?5, called = ?6
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called
                ],
            )? != 0)
        })
    }

    pub fn drop_sequence_row(&self, name: &str) -> Result<bool> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _sequences
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, &relation, RelationKind::Sequence)?;
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    pub fn load_sequence_rows(&self) -> Result<Vec<SequenceRow>> {
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, start, increment, current, called
                       FROM _sequences ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(SequenceRow {
                    relation: RelationIdentity::new(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ),
                    start: row.get(2)?,
                    increment: row.get(3)?,
                    current: row.get(4)?,
                    called: row.get(5)?,
                })
            })?;
            let mut sequences = Vec::new();
            for row in rows {
                sequences.push(row?);
            }
            Ok(sequences)
        })
    }

    /// Allocate one sequence value inside `SQLite` itself. `UPDATE RETURNING`
    /// is a single atomic statement, so no engine-side read/modify/write cache
    /// can race another connection.
    pub fn next_sequence_value(&self, name: &str) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let value = tx
                .query_row(
                    "UPDATE _sequences
                        SET current = CASE WHEN called = 0 THEN current
                                           ELSE current + increment END,
                            called = 1
                      WHERE schema_name = ?1 AND relation_name = ?2
                        AND (called = 0
                             OR (increment > 0 AND current <= ?3 - increment)
                             OR (increment < 0 AND current >= ?4 - increment))
                      RETURNING current",
                    params![relation.schema, relation.name, i64::MAX, i64::MIN],
                    |row| row.get(0),
                )
                .optional()?;
            if value.is_none() {
                let exists = tx
                    .query_row(
                        "SELECT 1 FROM _sequences
                          WHERE schema_name = ?1 AND relation_name = ?2",
                        params![relation.schema, relation.name],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if exists {
                    return Err(SQLiteError::StorageBackend(format!(
                        "sequence `{name}` overflow"
                    )));
                }
            }
            tx.commit()?;
            Ok(value)
        })
    }

    pub fn set_sequence_value(&self, name: &str, value: i64) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with(|connection| {
            Ok(connection
                .query_row(
                    "UPDATE _sequences SET current = ?3, called = 1
                     WHERE schema_name = ?1 AND relation_name = ?2 RETURNING current",
                    params![relation.schema, relation.name, value],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    pub fn save_view(&self, view: &ViewRow) -> Result<()> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            Self::claim_relation(&tx, &view.relation, RelationKind::View)?;
            tx.execute(
                "INSERT OR REPLACE INTO _views
                    (schema_name, relation_name, kind, definition_json)
                 VALUES (?1, ?2, 'view', ?3)",
                params![
                    view.relation.schema,
                    view.relation.name,
                    view.definition_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_view(&self, relation: &RelationIdentity) -> Result<bool> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _views WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, relation, RelationKind::View)?;
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    pub fn load_views(&self) -> Result<Vec<ViewRow>> {
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, definition_json
                   FROM _views ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(ViewRow {
                    relation: RelationIdentity::new(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ),
                    definition_json: row.get(2)?,
                })
            })?;
            let mut views = Vec::new();
            for row in rows {
                views.push(row?);
            }
            Ok(views)
        })
    }

    /// Register the existence of a named graph in the catalog.
    /// Matches UQA behavior for `Catalog.save_named_graph`.
    pub fn save_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _named_graphs (name) VALUES (?1)",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Drop the named-graph registry row plus every membership entry
    /// that scopes a vertex or edge to this graph. Vertex / edge rows
    /// stay in `_graph_vertices` / `_graph_edges` until they go
    /// orphan; call [`Catalog::purge_orphan_graph_entities`] after to
    /// GC them. Matches UQA behavior for `Catalog.drop_named_graph` plus the
    /// orphan sweep that the engine performs on its behalf.
    pub fn drop_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _named_graphs WHERE name = ?1", params![name])?;
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Sorted list of every persisted named graph.
    /// Matches UQA behavior for `Catalog.load_named_graphs`.
    pub fn load_named_graphs(&self) -> Result<Vec<String>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM _named_graphs ORDER BY name")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Persist a vertex by global id. `properties_json` is the JSON
    /// encoding of the property map. Matches UQA behavior for
    /// `Catalog.save_vertex` extended with the `label` column the
    /// `SQLiteGraphStore` writes alongside it.
    pub fn save_vertex(&self, vertex_id: u64, label: &str, properties_json: &str) -> Result<()> {
        let vertex_id = encode_catalog_id("vertex", vertex_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_vertices (vertex_id, label, properties_json) \
                 VALUES (?1, ?2, ?3)",
                params![vertex_id, label, properties_json],
            )?;
            Ok(())
        })
    }

    /// Delete a vertex by global id.
    pub fn delete_vertex(&self, vertex_id: u64) -> Result<()> {
        let vertex_id = encode_catalog_id("vertex", vertex_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices WHERE vertex_id = ?1",
                params![vertex_id],
            )?;
            Ok(())
        })
    }

    /// Every vertex row sorted by id, returned as
    /// `(vertex_id, label, properties_json)` so the caller rebuilds
    /// the `Vertex` from the typed columns plus the JSON-encoded
    /// property map. Matches UQA behavior for `Catalog.load_vertices`.
    pub fn load_vertices(&self) -> Result<Vec<(u64, String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT vertex_id, label, properties_json FROM _graph_vertices ORDER BY vertex_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, label, props) = row?;
                out.push((decode_catalog_id("vertex", id)?, label, props));
            }
            Ok(out)
        })
    }

    /// Persist an edge by global id with its source / target vertices,
    /// label, and JSON-encoded property map. Matches UQA behavior for
    /// `Catalog.save_edge`.
    pub fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> Result<()> {
        let edge_id = encode_catalog_id("edge", edge_id)?;
        let source_id = encode_catalog_id("edge source vertex", source_id)?;
        let target_id = encode_catalog_id("edge target vertex", target_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_edges \
                    (edge_id, source_id, target_id, label, properties_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![edge_id, source_id, target_id, label, properties_json],
            )?;
            Ok(())
        })
    }

    /// Delete an edge by global id.
    pub fn delete_edge(&self, edge_id: u64) -> Result<()> {
        let edge_id = encode_catalog_id("edge", edge_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_edges WHERE edge_id = ?1",
                params![edge_id],
            )?;
            Ok(())
        })
    }

    /// Every edge row sorted by id. Matches UQA behavior for `Catalog.load_edges`.
    pub fn load_edges(&self) -> Result<Vec<EdgeRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT edge_id, source_id, target_id, label, properties_json \
                   FROM _graph_edges ORDER BY edge_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, src, tgt, label, props) = row?;
                out.push(EdgeRow {
                    edge_id: decode_catalog_id("edge", id)?,
                    source_id: decode_catalog_id("edge source vertex", src)?,
                    target_id: decode_catalog_id("edge target vertex", tgt)?,
                    label,
                    properties_json: props,
                });
            }
            Ok(out)
        })
    }

    /// Attach `entity_id` (a vertex when `entity_type == "vertex"`, an
    /// edge when `"edge"`) to `graph_name`. The same entity can sit in
    /// many graphs; the row is keyed by the full triple so duplicate
    /// attaches no-op.
    pub fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        let entity_id = encode_catalog_id("graph membership entity", entity_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _graph_membership \
                    (entity_type, entity_id, graph_name) \
                 VALUES (?1, ?2, ?3)",
                params![entity_type, entity_id, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach `entity_id` from `graph_name`.
    pub fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        let entity_id = encode_catalog_id("graph membership entity", entity_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership \
                  WHERE entity_type = ?1 AND entity_id = ?2 AND graph_name = ?3",
                params![entity_type, entity_id, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach every entity from `graph_name`. Used as the prelude to a
    /// full graph drop / Cypher resync.
    pub fn delete_graph_membership_for_graph(&self, graph_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            Ok(())
        })
    }

    /// Every membership row, returned as `(entity_type, entity_id, graph_name)`.
    pub fn load_graph_memberships(&self) -> Result<Vec<(String, u64, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT entity_type, entity_id, graph_name FROM _graph_membership \
                  ORDER BY graph_name, entity_type, entity_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (ty, id, graph) = row?;
                out.push((ty, decode_catalog_id("graph membership entity", id)?, graph));
            }
            Ok(out)
        })
    }

    /// Drop vertex / edge rows that no membership row still references.
    /// Run after a detach / drop to garbage-collect orphaned entities.
    pub fn purge_orphan_graph_entities(&self) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices \
                  WHERE vertex_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'vertex' \
                  )",
                [],
            )?;
            c.execute(
                "DELETE FROM _graph_edges \
                  WHERE edge_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'edge' \
                  )",
                [],
            )?;
            Ok(())
        })
    }

    pub fn replace_named_graph(&self, graph_name: &str, snapshot: &GraphSnapshot) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "INSERT OR IGNORE INTO _named_graphs (name) VALUES (?1)",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _path_indexes
                  WHERE substr(graph_name, 1, length(?1) + 2) = ?1 || '::'",
                params![graph_name],
            )?;
            for vertex in &snapshot.vertices {
                let vertex_id = encode_catalog_id("vertex", vertex.vertex_id)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _graph_vertices
                        (vertex_id, label, properties_json) VALUES (?1, ?2, ?3)",
                    params![vertex_id, vertex.label, vertex.properties_json],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO _graph_membership
                        (entity_type, entity_id, graph_name) VALUES ('vertex', ?1, ?2)",
                    params![vertex_id, graph_name],
                )?;
            }
            for edge in &snapshot.edges {
                let edge_id = encode_catalog_id("edge", edge.edge_id)?;
                let source_id = encode_catalog_id("edge source vertex", edge.source_id)?;
                let target_id = encode_catalog_id("edge target vertex", edge.target_id)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _graph_edges
                        (edge_id, source_id, target_id, label, properties_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        edge_id,
                        source_id,
                        target_id,
                        edge.label,
                        edge.properties_json
                    ],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO _graph_membership
                        (entity_type, entity_id, graph_name) VALUES ('edge', ?1, ?2)",
                    params![edge_id, graph_name],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO _metadata (key, value) VALUES (?1, ?2)",
                params![
                    format!("graph_label_registry::{graph_name}"),
                    snapshot.label_registry_json
                ],
            )?;
            Self::purge_orphan_graph_entities_on(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_named_graph_data(&self, graph_name: &str) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _named_graphs WHERE name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _metadata WHERE key = ?1",
                params![format!("graph_label_registry::{graph_name}")],
            )?;
            tx.execute(
                "DELETE FROM _path_indexes
                  WHERE substr(graph_name, 1, length(?1) + 2) = ?1 || '::'",
                params![graph_name],
            )?;
            Self::purge_orphan_graph_entities_on(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    fn purge_orphan_graph_entities_on(c: &rusqlite::Connection) -> Result<()> {
        c.execute(
            "DELETE FROM _graph_vertices
              WHERE vertex_id NOT IN (
                SELECT entity_id FROM _graph_membership WHERE entity_type = 'vertex'
              )",
            [],
        )?;
        c.execute(
            "DELETE FROM _graph_edges
              WHERE edge_id NOT IN (
                SELECT entity_id FROM _graph_membership WHERE entity_type = 'edge'
              )",
            [],
        )?;
        Ok(())
    }

    // -- Named analyzers ---------------------------------------------------

    /// Persist a named analyzer configuration. Matches UQA behavior for
    /// `Catalog.save_analyzer`.
    pub fn save_analyzer(&self, name: &str, config_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _analyzers (name, config_json) VALUES (?1, ?2)",
                params![name, config_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_analyzer(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _analyzers WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_analyzers(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, config_json FROM _analyzers ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Per-field analyzer overrides --------------------------------------

    /// Persist a `(table, field, phase) -> analyzer_name` row. Mirrors
    /// Persist a table-field analyzer mapping.
    pub fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _table_field_analyzers \
                    (table_name, field, phase, analyzer_name) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![table_name, field, phase, analyzer_name],
            )?;
            Ok(())
        })
    }

    pub fn drop_table_field_analyzers(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _table_field_analyzers WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }

    pub fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _table_field_analyzers
                  WHERE table_name = ?1 AND field = ?2",
                params![table_name, field],
            )?;
            tx.execute(
                "INSERT INTO _table_field_analyzers
                    (table_name, field, phase, analyzer_name)
                 VALUES (?1, ?2, ?3, ?4)",
                params![table_name, field, phase, analyzer_name],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_table_field_analyzer_field(&self, table_name: &str, field: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _table_field_analyzers
                  WHERE table_name = ?1 AND field = ?2",
                params![table_name, field],
            )?;
            Ok(())
        })
    }

    /// Every `(table_name, field, phase, analyzer_name)` row sorted by
    /// `(table_name, field, phase)`.
    pub fn load_table_field_analyzers(&self) -> Result<Vec<(String, String, String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT table_name, field, phase, analyzer_name FROM _table_field_analyzers \
                  ORDER BY table_name, field, phase",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Foreign servers ---------------------------------------------------

    pub fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _foreign_servers (name, fdw_type, options) \
                 VALUES (?1, ?2, ?3)",
                params![name, fdw_type, options_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_foreign_server(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _foreign_servers WHERE name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn load_foreign_servers(&self) -> Result<Vec<(String, String, String)>> {
        self.conn.with(|c| {
            let mut stmt =
                c.prepare("SELECT name, fdw_type, options FROM _foreign_servers ORDER BY name")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Foreign tables ----------------------------------------------------

    pub fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, relation, RelationKind::ForeignTable)?;
            tx.execute(
                "INSERT OR REPLACE INTO _foreign_tables \
                    (schema_name, relation_name, kind, server_name, columns_json, options) \
                 VALUES (?1, ?2, 'foreign_table', ?3, ?4, ?5)",
                params![
                    relation.schema,
                    relation.name,
                    server_name,
                    columns_json,
                    options_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_foreign_table(&self, relation: &RelationIdentity) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _foreign_tables
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, relation, RelationKind::ForeignTable)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_foreign_tables(&self) -> Result<Vec<ForeignTableRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT schema_name, relation_name, server_name, columns_json, options
                   FROM _foreign_tables ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (schema, name, server, cols, opts) = row?;
                out.push(ForeignTableRow {
                    relation: RelationIdentity::new(schema, name),
                    server_name: server,
                    columns_json: cols,
                    options_json: opts,
                });
            }
            Ok(out)
        })
    }

    // -- Catalog indexes (CREATE INDEX state) ------------------------------

    pub fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _catalog_indexes \
                    (name, index_type, table_name, columns, parameters) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, index_type, table_name, columns_json, parameters_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_catalog_index(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _catalog_indexes WHERE name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn drop_catalog_indexes_for_table(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _catalog_indexes WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }

    pub fn load_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name, index_type, table_name, columns, parameters \
                   FROM _catalog_indexes ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (name, ty, table, cols, params_json) = row?;
                out.push(CatalogIndexRow {
                    name,
                    index_type: ty,
                    table_name: table,
                    columns_json: cols,
                    parameters_json: params_json,
                });
            }
            Ok(out)
        })
    }

    // -- Path indexes ------------------------------------------------------

    pub fn save_path_index(&self, graph_name: &str, label_sequences_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _path_indexes (graph_name, label_sequences) \
                 VALUES (?1, ?2)",
                params![graph_name, label_sequences_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_path_index(&self, graph_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _path_indexes WHERE graph_name = ?1",
                params![graph_name],
            )?;
            Ok(())
        })
    }

    /// `(graph_name, label_sequences_json)` for every persisted path index.
    pub fn load_path_indexes(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT graph_name, label_sequences FROM _path_indexes ORDER BY graph_name",
            )?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Persist a per-column ANALYZE summary so the planner still has
    /// cardinality / range estimates after a restart. `min_value` and
    /// `max_value` are stored as strings (JSON when the value isn't
    /// natively textual) so the column type is irrelevant.
    pub fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _column_stats
                    (table_name, column_name, distinct_count, null_count,
                     min_value, max_value, row_count,
                     histogram, mcv_values, mcv_frequencies)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    stats.table_name,
                    stats.column_name,
                    stats.distinct_count,
                    stats.null_count,
                    stats.min_value,
                    stats.max_value,
                    stats.row_count,
                    stats.histogram_json,
                    stats.mcv_values_json,
                    stats.mcv_frequencies_json,
                ],
            )?;
            Ok(())
        })
    }

    pub fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> Result<()> {
        if let Some(row) = stats.iter().find(|row| row.table_name != table_name) {
            return Err(SQLiteError::StorageBackend(format!(
                "column stats row for table `{}` cannot be stored in snapshot `{table_name}`",
                row.table_name
            )));
        }
        self.conn.with_mut(|connection| {
            let transaction = connection.savepoint()?;
            transaction.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1",
                params![table_name],
            )?;
            {
                let mut statement = transaction.prepare(
                    "INSERT INTO _column_stats
                        (table_name, column_name, distinct_count, null_count,
                         min_value, max_value, row_count,
                         histogram, mcv_values, mcv_frequencies)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )?;
                for row in stats {
                    statement.execute(params![
                        row.table_name,
                        row.column_name,
                        row.distinct_count,
                        row.null_count,
                        row.min_value,
                        row.max_value,
                        row.row_count,
                        row.histogram_json,
                        row.mcv_values_json,
                        row.mcv_frequencies_json,
                    ])?;
                }
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn load_column_stats(&self, table_name: &str) -> Result<Vec<ColumnStatsRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT column_name, distinct_count, null_count,
                        min_value, max_value, row_count,
                        histogram, mcv_values, mcv_frequencies
                   FROM _column_stats
                  WHERE table_name = ?1
                  ORDER BY column_name",
            )?;
            let rows = stmt.query_map(params![table_name], |r| {
                Ok(ColumnStatsRow {
                    column_name: r.get::<_, String>(0)?,
                    distinct_count: r.get::<_, i64>(1)?,
                    null_count: r.get::<_, i64>(2)?,
                    min_value: r.get::<_, Option<String>>(3)?,
                    max_value: r.get::<_, Option<String>>(4)?,
                    row_count: r.get::<_, i64>(5)?,
                    histogram_json: r.get::<_, String>(6)?,
                    mcv_values_json: r.get::<_, String>(7)?,
                    mcv_frequencies_json: r.get::<_, String>(8)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn delete_column_stats(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }
}

fn into_storage_result<T>(result: Result<T>) -> StorageBackendResult<T> {
    result.map_err(StorageBackendError::from)
}

impl CatalogFacade for Catalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::set_metadata(self, key, value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::get_metadata(self, key))
    }

    fn fts_storage_was_reset(&self) -> bool {
        self.fts_storage_was_reset
    }

    fn migrate_relation_namespace(&self) -> StorageBackendResult<()> {
        into_storage_result(self.conn.with(|connection| {
            let foreign_key_violation = connection
                .query_row("PRAGMA foreign_key_check", [], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()?;
            if let Some((table, row_id)) = foreign_key_violation {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation catalog foreign-key violation in `{table}` row {row_id}"
                )));
            }
            let orphan = connection
                .query_row(
                    "SELECT r.schema_name, r.relation_name, r.kind
                       FROM _relations AS r
                       LEFT JOIN (
                           SELECT schema_name, relation_name, 'table' AS kind FROM _tables
                           UNION ALL
                           SELECT schema_name, relation_name, 'view' AS kind FROM _views
                           UNION ALL
                           SELECT schema_name, relation_name, 'sequence' AS kind FROM _sequences
                           UNION ALL
                           SELECT schema_name, relation_name, 'foreign_table' AS kind
                             FROM _foreign_tables
                       ) AS child
                         ON child.schema_name = r.schema_name
                        AND child.relation_name = r.relation_name
                        AND child.kind = r.kind
                      WHERE child.relation_name IS NULL
                      LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((schema, name, kind)) = orphan {
                return Err(SQLiteError::StorageBackend(format!(
                    "catalog relation `{schema}.{name}` has no {kind} child"
                )));
            }
            Ok(())
        }))
    }

    fn save_schema(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_schema(self, name))
    }

    fn drop_schema(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_schema(self, name))
    }

    fn load_schemas(&self) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::load_schemas(self))
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table(self, schema))
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        into_storage_result(Catalog::load_tables(self))
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table(self, name))
    }

    fn drop_table_and_data(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_and_data(self, name))
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_table_data(self, name))
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_table_data(self, from, to))
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_column_data(self, table_name, column_name))
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_column_data(self, table_name, from, to))
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_model(self, name, json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_models(self))
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_model(self, name))
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_model(self, name))
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_scoring_params(self, name, params_json))
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_scoring_params(self, name))
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_all_scoring_params(self))
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_scoring_params(self, name))
    }

    fn create_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::create_sequence_row(self, sequence))
    }

    fn replace_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::replace_sequence_row(self, sequence))
    }

    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::drop_sequence_row(self, name))
    }

    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        into_storage_result(Catalog::load_sequence_rows(self))
    }

    fn next_sequence_value(&self, name: &str) -> StorageBackendResult<Option<i64>> {
        into_storage_result(Catalog::next_sequence_value(self, name))
    }

    fn set_sequence_value(&self, name: &str, value: i64) -> StorageBackendResult<Option<i64>> {
        into_storage_result(Catalog::set_sequence_value(self, name, value))
    }

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_view(self, view))
    }

    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::drop_view(self, relation))
    }

    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        into_storage_result(Catalog::load_views(self))
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_named_graph(self, name))
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_named_graph(self, name))
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::load_named_graphs(self))
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_vertex(
            self,
            vertex_id,
            label,
            properties_json,
        ))
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_vertex(self, vertex_id))
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        into_storage_result(Catalog::load_vertices(self))
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_edge(
            self,
            edge_id,
            source_id,
            target_id,
            label,
            properties_json,
        ))
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_edge(self, edge_id))
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        into_storage_result(Catalog::load_edges(self))
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership_for_graph(self, graph_name))
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        into_storage_result(Catalog::load_graph_memberships(self))
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_orphan_graph_entities(self))
    }

    fn replace_named_graph(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_named_graph(self, graph_name, snapshot))
    }

    fn drop_named_graph_data(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_named_graph_data(self, graph_name))
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_analyzer(self, name, config_json))
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_analyzer(self, name))
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_analyzers(self))
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table_field_analyzer(
            self,
            table_name,
            field,
            phase,
            analyzer_name,
        ))
    }

    fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_table_field_analyzer(
            self,
            table_name,
            field,
            phase,
            analyzer_name,
        ))
    }

    fn drop_table_field_analyzer_field(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_field_analyzer_field(
            self, table_name, field,
        ))
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_field_analyzers(self, table_name))
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        into_storage_result(Catalog::load_table_field_analyzers(self))
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_server(
            self,
            name,
            fdw_type,
            options_json,
        ))
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_server(self, name))
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        into_storage_result(Catalog::load_foreign_servers(self))
    }

    fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_table(
            self,
            relation,
            server_name,
            columns_json,
            options_json,
        ))
    }

    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_table(self, relation))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        into_storage_result(Catalog::load_foreign_tables(self))
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_catalog_index(
            self,
            name,
            index_type,
            table_name,
            columns_json,
            parameters_json,
        ))
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_index(self, name))
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_indexes_for_table(self, table_name))
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        into_storage_result(Catalog::load_catalog_indexes(self))
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_path_index(
            self,
            graph_name,
            label_sequences_json,
        ))
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_path_index(self, graph_name))
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_path_indexes(self))
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_column_stats(self, stats))
    }

    fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_column_stats(self, table_name, stats))
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        into_storage_result(Catalog::load_column_stats(self, table_name))
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_column_stats(self, table_name))
    }
}

/// Migrations applied in order. Each `(version, sql)` is run in a single
/// transaction; the `_meta.schema_version` row is bumped on success.
const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r"
    CREATE TABLE IF NOT EXISTS _tables (
        name           TEXT PRIMARY KEY,
        analyzer       TEXT NOT NULL,
        fts_fields     TEXT NOT NULL,
        vector_fields  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _documents (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        body       TEXT NOT NULL,
        PRIMARY KEY (table_name, doc_id)
    );

    CREATE TABLE IF NOT EXISTS _postings (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        term       TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        positions  BLOB NOT NULL,
        PRIMARY KEY (table_name, field, term, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _postings_doc_idx
        ON _postings (table_name, doc_id);

    CREATE TABLE IF NOT EXISTS _doc_lengths (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        field      TEXT NOT NULL,
        length     INTEGER NOT NULL,
        PRIMARY KEY (table_name, doc_id, field)
    );

    CREATE TABLE IF NOT EXISTS _field_stats (
        table_name   TEXT NOT NULL,
        field        TEXT NOT NULL,
        total_length INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _vectors (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        vector     BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    ",
    ),
    (
        2,
        r"
    CREATE TABLE IF NOT EXISTS _models (
        name TEXT PRIMARY KEY,
        body TEXT NOT NULL
    );
    ",
    ),
    (
        3,
        r"
    ALTER TABLE _tables ADD COLUMN columns TEXT;
    ",
    ),
    (
        4,
        r"
    CREATE TABLE IF NOT EXISTS _scoring_params (
        name TEXT PRIMARY KEY,
        params TEXT NOT NULL
    );
    ",
    ),
    (
        5,
        r"
    CREATE TABLE IF NOT EXISTS _graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        graph     TEXT NOT NULL,
        vertex_id INTEGER NOT NULL,
        body      TEXT NOT NULL,
        PRIMARY KEY (graph, vertex_id)
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        graph   TEXT NOT NULL,
        edge_id INTEGER NOT NULL,
        body    TEXT NOT NULL,
        PRIMARY KEY (graph, edge_id)
    );
    CREATE INDEX IF NOT EXISTS _graph_edges_by_graph
        ON _graph_edges (graph);
    ",
    ),
    // Re-shape graph storage to mirror the canonical UQA implementation's UQA `storage/catalog`:
    // global vertex / edge tables keyed by id, a separate
    // `_graph_membership` table mapping each entity to one or more
    // named graphs, and the four supporting indexes the planner needs
    // for label-based lookups. The legacy v5 tables (denormalized by
    // graph name + JSON body) get dropped because no engine call site
    // reads them anymore.
    (
        6,
        r"
    DROP TABLE IF EXISTS _graphs;
    DROP TABLE IF EXISTS _graph_vertices;
    DROP TABLE IF EXISTS _graph_edges;

    CREATE TABLE IF NOT EXISTS _named_graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        vertex_id       INTEGER PRIMARY KEY,
        label           TEXT NOT NULL DEFAULT '',
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        edge_id         INTEGER PRIMARY KEY,
        source_id       INTEGER NOT NULL,
        target_id       INTEGER NOT NULL,
        label           TEXT NOT NULL,
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_membership (
        entity_type TEXT NOT NULL,
        entity_id   INTEGER NOT NULL,
        graph_name  TEXT NOT NULL,
        PRIMARY KEY (entity_type, entity_id, graph_name)
    );

    CREATE INDEX IF NOT EXISTS _graph_vertices_label
        ON _graph_vertices (label);
    CREATE INDEX IF NOT EXISTS _graph_edges_out
        ON _graph_edges (source_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_in
        ON _graph_edges (target_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_label
        ON _graph_edges (label);
    ",
    ),
    // Persist the five engine-side registries that previously lived
    // only in `Engine`'s in-memory maps (named analyzers, table-field
    // analyzer overrides, foreign servers / tables, registered
    // indexes, graph path indexes). Tables and column shapes mirror
    // the canonical UQA implementation's UQA `storage/catalog` exactly.
    (
        7,
        r"
    CREATE TABLE IF NOT EXISTS _analyzers (
        name        TEXT PRIMARY KEY,
        config_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _table_field_analyzers (
        table_name    TEXT NOT NULL,
        field         TEXT NOT NULL,
        phase         TEXT NOT NULL,
        analyzer_name TEXT NOT NULL,
        PRIMARY KEY (table_name, field, phase)
    );

    CREATE TABLE IF NOT EXISTS _foreign_servers (
        name     TEXT PRIMARY KEY,
        fdw_type TEXT NOT NULL,
        options  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _foreign_tables (
        name         TEXT PRIMARY KEY,
        server_name  TEXT NOT NULL,
        columns_json TEXT NOT NULL,
        options      TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _catalog_indexes (
        name       TEXT PRIMARY KEY,
        index_type TEXT NOT NULL,
        table_name TEXT NOT NULL,
        columns    TEXT NOT NULL,
        parameters TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _path_indexes (
        graph_name      TEXT PRIMARY KEY,
        label_sequences TEXT NOT NULL
    );
    ",
    ),
    // Persist per-column statistics produced by ANALYZE so that the
    // optimiser still has cardinality / range estimates after a
    // restart. Mirrors the canonical UQA implementation's `_column_stats` table.
    (
        8,
        r"
    CREATE TABLE IF NOT EXISTS _column_stats (
        table_name      TEXT NOT NULL,
        column_name     TEXT NOT NULL,
        distinct_count  INTEGER NOT NULL,
        null_count      INTEGER NOT NULL,
        min_value       TEXT,
        max_value       TEXT,
        row_count       INTEGER NOT NULL,
        histogram       TEXT NOT NULL DEFAULT '[]',
        mcv_values      TEXT NOT NULL DEFAULT '[]',
        mcv_frequencies TEXT NOT NULL DEFAULT '[]',
        PRIMARY KEY (table_name, column_name)
    );
    ",
    ),
    (
        9,
        r"
    CREATE TABLE IF NOT EXISTS _ivf_indexes (
        table_name          TEXT NOT NULL,
        field               TEXT NOT NULL,
        dimensions          INTEGER NOT NULL,
        nlist               INTEGER NOT NULL,
        nprobe              INTEGER NOT NULL,
        train_threshold     INTEGER NOT NULL,
        state               TEXT NOT NULL,
        trained_size        INTEGER NOT NULL,
        deletes_since_train INTEGER NOT NULL,
        vector_count        INTEGER NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _ivf_centroids (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        centroid_id INTEGER NOT NULL,
        vector      BLOB NOT NULL,
        PRIMARY KEY (table_name, field, centroid_id)
    );

    CREATE TABLE IF NOT EXISTS _ivf_assignments (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        doc_id      INTEGER NOT NULL,
        centroid_id INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id);
    ",
    ),
    (
        10,
        r"
    CREATE TABLE IF NOT EXISTS _vectors_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        vector         BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _vectors_v10
        (table_name, field, doc_id, vector_ordinal, vector)
        SELECT table_name, field, doc_id, 0, vector FROM _vectors;
    DROP TABLE IF EXISTS _vectors;
    ALTER TABLE _vectors_v10 RENAME TO _vectors;

    CREATE TABLE IF NOT EXISTS _ivf_assignments_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        centroid_id    INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _ivf_assignments_v10
        (table_name, field, doc_id, vector_ordinal, centroid_id)
        SELECT table_name, field, doc_id, 0, centroid_id FROM _ivf_assignments;
    DROP TABLE IF EXISTS _ivf_assignments;
    ALTER TABLE _ivf_assignments_v10 RENAME TO _ivf_assignments;
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id, vector_ordinal);
    ",
    ),
    // Map logical btree indexes to compact durable postings. The engine
    // hydrates its in-memory B-tree from these rows on reopen instead of
    // reparsing every full document on the first indexed predicate.
    (
        11,
        r"
    CREATE TABLE IF NOT EXISTS _btree_indexes (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _btree_index_entries (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        value_json TEXT NOT NULL,
        PRIMARY KEY (table_name, field, doc_id),
        FOREIGN KEY (table_name, field)
            REFERENCES _btree_indexes (table_name, field)
            ON UPDATE CASCADE ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS _btree_index_value_idx
        ON _btree_index_entries (table_name, field, value_json, doc_id);
    CREATE INDEX IF NOT EXISTS _btree_index_doc_idx
        ON _btree_index_entries (table_name, doc_id);
    ",
    ),
    // `_postings` already has a unique auto-index over
    // `(table_name, field, term, doc_id)`. Its first three columns cover
    // term lookup, so the former `_postings_term_idx` duplicated every FTS
    // write without enabling a distinct access path.
    (
        12,
        r"
    DROP INDEX IF EXISTS _postings_term_idx;
    ",
    ),
    (
        13,
        r"
    CREATE TABLE IF NOT EXISTS _schemas (
        name TEXT PRIMARY KEY
    );
    INSERT OR IGNORE INTO _schemas (name) VALUES ('public');
    ",
    ),
    (
        14,
        r"
    CREATE TABLE IF NOT EXISTS _sequences (
        name      TEXT PRIMARY KEY,
        start     INTEGER NOT NULL,
        increment INTEGER NOT NULL,
        current   INTEGER NOT NULL
    );
    ",
    ),
    // Keep large binary and numeric document values out of JSON bodies. This
    // table used to be created lazily by document reads and writes, which
    // turned a read into schema-changing DDL and serialized WAL readers behind
    // an active writer. Catalog migration now guarantees its existence before
    // any document store is exposed.
    (
        15,
        r"
    CREATE TABLE IF NOT EXISTS _document_blobs (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        field_name TEXT NOT NULL,
        bytes      BLOB NOT NULL,
        PRIMARY KEY (table_name, doc_id, field_name)
    );
    ",
    ),
    // Persist table CHECK / FOREIGN KEY / composite PRIMARY KEY and UNIQUE
    // metadata. Existing catalogs receive an empty payload which the engine
    // interprets as the pre-v16 default constraint set.
    (
        16,
        r"
    ALTER TABLE _tables ADD COLUMN constraints TEXT NOT NULL DEFAULT '';
    ",
    ),
    // Replace flat relation-name strings with a shared schema-owned relation
    // catalog. The data rewrite and collision preflight are implemented in
    // Rust so legacy view/sequence JSON can migrate in the same transaction.
    (17, ""),
    // A sequence needs an explicit first-allocation bit.  The former
    // `current = start - increment` sentinel cannot represent valid BIGINT
    // boundary starts. Existing rows use the old sentinel representation, so
    // `called = 1` preserves their next-value behavior exactly.
    (
        18,
        r"
    ALTER TABLE _sequences
        ADD COLUMN called INTEGER NOT NULL DEFAULT 1 CHECK (called IN (0, 1));
    ",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Catalog {
        let mc = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(mc).unwrap()
    }

    #[test]
    fn migration_creates_tables_table() {
        let cat = fresh();
        cat.conn
            .with(|c| {
                let count: u32 = c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = '_tables'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 1);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn save_load_round_trip() {
        let cat = fresh();
        let schema = TableSchema {
            relation: RelationIdentity::new("public", "articles"),
            analyzer_json:
                "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                    .into(),
            fts_fields: vec!["title".into(), "body".into()],
            vector_fields: vec![VectorFieldSchema {
                field: "embedding".into(),
                dimensions: 768,
            }],
            columns_json: String::new(),
            constraints_json: r#"{"checks":[],"foreign_keys":[],"key_constraints":[]}"#.into(),
        };
        cat.save_table(&schema).unwrap();
        let loaded = cat.load_tables().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].relation.qualified_name(), "public.articles");
        assert_eq!(loaded[0].fts_fields, vec!["title", "body"]);
        assert_eq!(loaded[0].vector_fields.len(), 1);
        assert_eq!(loaded[0].vector_fields[0].field, "embedding");
        assert_eq!(loaded[0].vector_fields[0].dimensions, 768);
        assert!(loaded[0].columns_json.is_empty());
        assert_eq!(loaded[0].constraints_json, schema.constraints_json);
    }

    #[test]
    fn catalog_facade_trait_object_round_trips_table() {
        let cat = fresh();
        let facade: &dyn CatalogFacade = &cat;
        let schema = TableSchema {
            relation: RelationIdentity::new("public", "facade_articles"),
            analyzer_json:
                "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                    .into(),
            fts_fields: vec!["title".into()],
            vector_fields: vec![VectorFieldSchema {
                field: "embedding".into(),
                dimensions: 128,
            }],
            columns_json: String::new(),
            constraints_json: String::new(),
        };
        facade.save_table(&schema).unwrap();
        let loaded = facade.load_tables().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].relation.qualified_name(),
            "public.facade_articles"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat1 = Catalog::open(mc.clone()).unwrap();
        // Reopen on the same handle: should not re-run migrations or
        // raise an error.
        let _cat2 = Catalog::open(mc).unwrap();
    }

    #[test]
    fn migration_15_creates_document_blob_storage_for_existing_catalogs() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _current = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            conn.execute("DROP TABLE _document_blobs", [])?;
            conn.execute(
                "UPDATE _metadata SET value = '14' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let _upgraded = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            let count: u32 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = '_document_blobs'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_16_adds_backward_compatible_table_constraints() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let current = Catalog::open(mc.clone()).unwrap();
        current
            .save_table(&TableSchema {
                relation: RelationIdentity::new("public", "legacy"),
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
        drop(current);
        mc.with(|conn| {
            conn.execute("ALTER TABLE _tables DROP COLUMN constraints", [])?;
            conn.execute(
                "UPDATE _metadata SET value = '15' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let upgraded = Catalog::open(mc).unwrap();
        let schemas = upgraded.load_tables().unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].relation.qualified_name(), "public.legacy");
        assert!(schemas[0].constraints_json.is_empty());
    }

    #[test]
    fn migration_18_preserves_legacy_sequence_sentinel_semantics() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let current = Catalog::open(connection.clone()).unwrap();
        current
            .create_sequence_row(&SequenceRow {
                relation: RelationIdentity::new("public", "legacy_uncalled"),
                start: 1,
                increment: 1,
                current: 0,
                called: false,
            })
            .unwrap();
        drop(current);
        connection
            .with(|conn| {
                conn.execute("ALTER TABLE _sequences DROP COLUMN called", [])?;
                conn.execute(
                    "UPDATE _metadata SET value = '17' WHERE key = 'schema_version'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let upgraded = Catalog::open(connection.clone()).unwrap();
        let row = upgraded.load_sequence_rows().unwrap().remove(0);
        assert!(
            row.called,
            "legacy current values are already sentinel-adjusted"
        );
        assert_eq!(
            upgraded
                .next_sequence_value("public.legacy_uncalled")
                .unwrap(),
            Some(1)
        );
        connection
            .with(|conn| {
                let version: String = conn.query_row(
                    "SELECT value FROM _metadata WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn corrupt_schema_version_is_reported_instead_of_replaying_migrations() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _current = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            conn.execute(
                "UPDATE _metadata SET value = 'not-a-version' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let error = Catalog::open(mc).err();
        assert!(matches!(
            error,
            Some(SQLiteError::InvalidSchemaVersion(version)) if version == "not-a-version"
        ));
    }

    #[test]
    fn future_schema_version_is_rejected() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _current = Catalog::open(mc.clone()).unwrap();
        let future = CURRENT_SCHEMA_VERSION + 1;
        mc.with(|conn| {
            conn.execute(
                "UPDATE _metadata SET value = ?1 WHERE key = 'schema_version'",
                [future.to_string()],
            )?;
            Ok(())
        })
        .unwrap();

        assert!(matches!(
            Catalog::open(mc).err(),
            Some(SQLiteError::UnsupportedSchemaVersion { found, supported })
                if found == future && supported == CURRENT_SCHEMA_VERSION
        ));
    }

    #[test]
    fn corrupt_catalog_index_columns_abort_column_lifecycle() {
        let cat = fresh();
        cat.save_catalog_index("broken", "btree", "docs", "not-json", "{}")
            .unwrap();

        assert!(matches!(
            cat.drop_column_data("docs", "title"),
            Err(SQLiteError::Serde(_))
        ));
        assert_eq!(cat.load_catalog_indexes().unwrap().len(), 1);
        assert!(matches!(
            cat.rename_column_data("docs", "title", "headline"),
            Err(SQLiteError::Serde(_))
        ));
        assert_eq!(
            cat.load_catalog_indexes().unwrap()[0].columns_json,
            "not-json"
        );
    }

    #[test]
    fn negative_graph_ids_are_reported_as_catalog_corruption() {
        let cat = fresh();
        cat.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _graph_vertices (vertex_id, label, properties_json)
                     VALUES (-1, 'person', '{}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            cat.load_vertices(),
            Err(SQLiteError::StorageBackend(message))
                if message.contains("negative vertex id -1")
        ));

        cat.conn
            .with(|connection| {
                connection.execute("DELETE FROM _graph_vertices", [])?;
                connection.execute(
                    "INSERT INTO _graph_edges
                        (edge_id, source_id, target_id, label, properties_json)
                     VALUES (1, -2, 3, 'knows', '{}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            cat.load_edges(),
            Err(SQLiteError::StorageBackend(message))
                if message.contains("negative edge source vertex id -2")
        ));

        cat.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _graph_membership (entity_type, entity_id, graph_name)
                     VALUES ('vertex', -3, 'g')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            cat.load_graph_memberships(),
            Err(SQLiteError::StorageBackend(message))
                if message.contains("negative graph membership entity id -3")
        ));
    }

    #[test]
    fn graph_ids_beyond_sqlite_integer_range_are_rejected_before_write() {
        let cat = fresh();

        assert!(matches!(
            cat.save_vertex(u64::MAX, "person", "{}"),
            Err(SQLiteError::StorageBackend(message))
                if message.contains("exceeds the SQLite INTEGER range")
        ));
        assert!(cat.load_vertices().unwrap().is_empty());
    }

    #[test]
    fn migration_drops_redundant_postings_term_index() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _current = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            conn.execute(
                "CREATE INDEX _postings_term_idx
                 ON _postings (table_name, field, term)",
                [],
            )?;
            conn.execute(
                "UPDATE _metadata SET value = '11' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let _migrated = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            let term_index_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = '_postings_term_idx'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(term_index_count, 0);

            let plan: String = conn.query_row(
                "EXPLAIN QUERY PLAN
                 SELECT doc_id, positions FROM _postings
                 WHERE table_name = 'docs' AND field = 'body' AND term = 'rust'
                 ORDER BY doc_id",
                [],
                |row| row.get(3),
            )?;
            assert!(
                plan.contains("sqlite_autoindex__postings_1"),
                "term lookup must use the composite primary-key index: {plan}"
            );
            Ok(())
        })
        .unwrap();
    }

    fn legacy_v16_catalog(table_names: &[&str], sequence_names: &[&str]) -> ManagedConnection {
        let connection = ManagedConnection::open_in_memory().unwrap();
        connection
            .with(|conn| {
                conn.execute_batch(
                    "CREATE TABLE _metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                     INSERT INTO _metadata(key, value) VALUES ('schema_version', '16');
                     CREATE TABLE _schemas (name TEXT PRIMARY KEY);
                     INSERT INTO _schemas(name) VALUES ('public');
                     CREATE TABLE _tables (
                         name TEXT PRIMARY KEY,
                         analyzer TEXT NOT NULL,
                         fts_fields TEXT NOT NULL,
                         vector_fields TEXT NOT NULL,
                         columns TEXT,
                         constraints TEXT NOT NULL DEFAULT ''
                     );
                     CREATE TABLE _sequences (
                         name TEXT PRIMARY KEY,
                         start INTEGER NOT NULL,
                         increment INTEGER NOT NULL,
                         current INTEGER NOT NULL
                     );
                     CREATE TABLE _foreign_tables (
                         name TEXT PRIMARY KEY,
                         server_name TEXT NOT NULL,
                         columns_json TEXT NOT NULL,
                         options TEXT NOT NULL
                     );
                     CREATE TABLE _documents (
                         table_name TEXT NOT NULL,
                         doc_id INTEGER NOT NULL,
                         body TEXT NOT NULL,
                         PRIMARY KEY(table_name, doc_id)
                     );",
                )?;
                for name in table_names {
                    conn.execute(
                        "INSERT INTO _tables
                            (name, analyzer, fts_fields, vector_fields, columns, constraints)
                         VALUES (?1, '{}', '[]', '[]', '[]', '')",
                        params![name],
                    )?;
                }
                for name in sequence_names {
                    conn.execute(
                        "INSERT INTO _sequences(name, start, increment, current)
                         VALUES (?1, 1, 1, 0)",
                        params![name],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        connection
    }

    #[test]
    fn relation_namespace_migration_is_atomic_and_moves_public_table_data() {
        let connection = legacy_v16_catalog(&["docs"], &["seq"]);
        connection
            .with(|conn| {
                conn.execute(
                    "INSERT INTO _documents(table_name, doc_id, body)
                     VALUES ('docs', 1, '{}')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO _foreign_tables(name, server_name, columns_json, options)
                     VALUES ('app.remote', 'server', '[]', '{}')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO _metadata(key, value)
                     VALUES ('sql_views_json', '{\"report\":{\"plan\":1}}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let catalog = Catalog::open(connection.clone()).unwrap();
        assert_eq!(
            catalog.load_tables().unwrap()[0].relation,
            RelationIdentity::new("public", "docs")
        );
        assert_eq!(
            catalog.load_sequence_rows().unwrap()[0].relation,
            RelationIdentity::new("public", "seq")
        );
        assert_eq!(
            catalog.load_foreign_tables().unwrap()[0].relation,
            RelationIdentity::new("app", "remote")
        );
        assert_eq!(
            catalog.load_views().unwrap()[0].relation,
            RelationIdentity::new("public", "report")
        );
        assert!(catalog.load_schemas().unwrap().contains(&"app".to_string()));
        connection
            .with(|conn| {
                let table_name: String = conn.query_row(
                    "SELECT table_name FROM _documents WHERE doc_id = 1",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(table_name, "public.docs");
                let version: String = conn.query_row(
                    "SELECT value FROM _metadata WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn relation_namespace_migration_moves_btree_parent_and_entries_without_fk_cascade() {
        let connection = legacy_v16_catalog(&["docs"], &[]);
        connection
            .with(|conn| {
                conn.execute_batch(
                    "CREATE TABLE _btree_indexes (
                         table_name TEXT NOT NULL,
                         field TEXT NOT NULL,
                         PRIMARY KEY (table_name, field)
                     );
                     CREATE TABLE _btree_index_entries (
                         table_name TEXT NOT NULL,
                         field TEXT NOT NULL,
                         doc_id INTEGER NOT NULL,
                         value_json TEXT NOT NULL,
                         PRIMARY KEY (table_name, field, doc_id),
                         FOREIGN KEY (table_name, field)
                             REFERENCES _btree_indexes (table_name, field)
                             ON UPDATE CASCADE ON DELETE CASCADE
                     );
                     INSERT INTO _btree_indexes(table_name, field)
                         VALUES ('docs', 'id');
                     INSERT INTO _btree_index_entries
                         (table_name, field, doc_id, value_json)
                         VALUES ('docs', 'id', 1, '{\"type\":\"Int\",\"value\":1}');",
                )?;
                // The migration must preserve the child even for a connection
                // that did not enable SQLite's optional FK enforcement.
                conn.pragma_update(None, "foreign_keys", "OFF")?;
                Ok(())
            })
            .unwrap();

        let _catalog = Catalog::open(connection.clone()).unwrap();
        connection
            .with(|conn| {
                conn.pragma_update(None, "foreign_keys", "ON")?;
                for table in ["_btree_indexes", "_btree_index_entries"] {
                    let canonical: i64 = conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE table_name = 'public.docs'"),
                        [],
                        |row| row.get(0),
                    )?;
                    let legacy: i64 = conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE table_name = 'docs'"),
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(canonical, 1, "canonical rows in {table}");
                    assert_eq!(legacy, 0, "legacy rows in {table}");
                }
                let violation = conn
                    .query_row("PRAGMA foreign_key_check", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()?;
                assert!(violation.is_none(), "foreign-key violation: {violation:?}");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn table_and_column_rename_move_btree_children_without_fk_cascade() {
        let catalog = fresh();
        catalog
            .save_table(&TableSchema {
                relation: RelationIdentity::new("public", "docs"),
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
        catalog
            .conn
            .with(|conn| {
                conn.execute(
                    "INSERT INTO _btree_indexes(table_name, field)
                     VALUES ('public.docs', 'id')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO _btree_index_entries
                         (table_name, field, doc_id, value_json)
                     VALUES ('public.docs', 'id', 1, '{\"type\":\"Int\",\"value\":1}')",
                    [],
                )?;
                conn.pragma_update(None, "foreign_keys", "OFF")?;
                Ok(())
            })
            .unwrap();

        catalog
            .rename_table_data("public.docs", "public.archived")
            .unwrap();
        catalog
            .rename_column_data("public.archived", "id", "item_id")
            .unwrap();

        catalog
            .conn
            .with(|conn| {
                conn.pragma_update(None, "foreign_keys", "ON")?;
                for table in ["_btree_indexes", "_btree_index_entries"] {
                    let moved: i64 = conn.query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {table}
                             WHERE table_name = 'public.archived' AND field = 'item_id'"
                        ),
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(moved, 1, "renamed rows in {table}");
                }
                let violation = conn
                    .query_row("PRAGMA foreign_key_check", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()?;
                assert!(violation.is_none(), "foreign-key violation: {violation:?}");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn relation_namespace_migration_rejects_alias_and_cross_kind_collisions() {
        for connection in [
            legacy_v16_catalog(&["docs", "public.docs"], &[]),
            legacy_v16_catalog(&["docs"], &["public.docs"]),
        ] {
            let error = Catalog::open(connection.clone()).err().unwrap();
            assert!(error.to_string().contains("migration collision"));
            assert!(error.to_string().contains("public.docs"));
            connection
                .with(|conn| {
                    let version: String = conn.query_row(
                        "SELECT value FROM _metadata WHERE key = 'schema_version'",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(version, "16");
                    let relation_table_count: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM sqlite_master
                         WHERE type = 'table' AND name = '_relations'",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(relation_table_count, 0);
                    Ok(())
                })
                .unwrap();
        }
    }
}
