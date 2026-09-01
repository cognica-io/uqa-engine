//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog version 17 relation-namespace migration.

use super::super::super::{
    drop_fts_aux_tables_for_table, params, quote_sql_identifier, table_exists,
    update_btree_table_name_rows_if_exists, update_table_name_rows_if_exists, Catalog,
    OptionalExtension, RelationIdentity, RelationKind, Result, SQLiteError,
    LEGACY_SEQUENCES_METADATA_KEY, LEGACY_VIEWS_METADATA_KEY,
};

#[derive(serde::Deserialize)]
struct LegacySequenceState {
    start: i64,
    increment: i64,
    current: i64,
}

pub(in crate::sqlite::catalog::migration) fn migration_relation(
    value: &str,
) -> Result<RelationIdentity> {
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
        "_hnsw_indexes",
        "_hnsw_nodes",
        "_hnsw_edges",
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

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let relation_namespace_already_present = Catalog::table_columns(tx, "_tables")?
        .is_some_and(|columns| columns.contains_key("schema_name"))
        && Catalog::table_columns(tx, "_relations")?.is_some()
        && Catalog::table_columns(tx, "_views")?.is_some();
    if relation_namespace_already_present {
        return Ok(());
    }
    let migrations = collect_sqlite_relation_migrations(tx)?;
    create_structural_relation_tables(tx)?;
    insert_relation_parents(tx, &migrations.seen)?;
    insert_relation_children(tx, &migrations)?;
    finish_sqlite_relation_migration(tx)
}
