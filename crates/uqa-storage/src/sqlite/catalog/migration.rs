//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog bootstrap, legacy namespace migration, and schema-shape repair.

use super::{
    drop_fts_aux_tables_for_table, params, quote_sql_identifier, table_exists,
    update_btree_table_name_rows_if_exists, update_table_name_rows_if_exists, Catalog,
    ManagedConnection, OptionalExtension, RelationIdentity, RelationKind, Result, SQLiteError,
    CURRENT_SCHEMA_VERSION, LEGACY_SEQUENCES_METADATA_KEY, LEGACY_VIEWS_METADATA_KEY, MIGRATIONS,
};
use crate::clustered_postings::{cluster_id, encode_cluster, encode_terms, ClusterPosting};

pub(super) fn encode_catalog_id(kind: &str, id: u64) -> Result<i64> {
    i64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} id {id} exceeds the SQLite INTEGER range"))
    })
}

pub(super) fn decode_catalog_id(kind: &str, id: i64) -> Result<u64> {
    u64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt catalog: negative {kind} id {id}"))
    })
}

fn decode_legacy_positions(blob: &[u8]) -> Result<Vec<u32>> {
    if !blob.len().is_multiple_of(std::mem::size_of::<u32>()) {
        return Err(SQLiteError::StorageBackend(
            "cannot migrate malformed legacy posting positions".into(),
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn insert_migrated_cluster(
    tx: &rusqlite::Transaction<'_>,
    cluster: (String, String, String, u64, Vec<ClusterPosting>),
) -> Result<()> {
    let (table, field, term, cluster_id, postings) = cluster;
    let (score_blob, positions_blob) = encode_cluster(&postings)
        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    tx.execute(
        "INSERT INTO _posting_clusters_v22
            (table_name, field, term, cluster_id, posting_count,
             score_blob, positions_blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            table,
            field,
            term,
            encode_catalog_id("posting cluster", cluster_id)?,
            encode_catalog_id("posting count", postings.len() as u64)?,
            score_blob,
            positions_blob
        ],
    )?;
    Ok(())
}

fn insert_migrated_document_terms(
    tx: &rusqlite::Transaction<'_>,
    document: (String, i64, String, Vec<String>),
) -> Result<()> {
    let (table, doc_id, field, terms) = document;
    let terms_blob =
        encode_terms(&terms).map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    tx.execute(
        "INSERT INTO _posting_documents_v22 (table_name, doc_id, field, terms_blob)
         VALUES (?1, ?2, ?3, ?4)",
        params![table, doc_id, field, terms_blob],
    )?;
    Ok(())
}

fn migrate_legacy_clusters_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT posting.table_name, posting.field, posting.term,
                posting.doc_id, posting.positions, lengths.length
           FROM _postings AS posting
           LEFT JOIN _doc_lengths AS lengths
             ON lengths.table_name = posting.table_name
            AND lengths.doc_id = posting.doc_id
            AND lengths.field = posting.field
          ORDER BY posting.table_name, posting.field, posting.term,
                   posting.doc_id",
    )?;
    let mut rows = statement.query([])?;
    let mut group: Option<(String, String, String, u64, Vec<ClusterPosting>)> = None;
    while let Some(row) = rows.next()? {
        let table = row.get::<_, String>(0)?;
        let field = row.get::<_, String>(1)?;
        let term = row.get::<_, String>(2)?;
        let stored_doc_id = row.get::<_, i64>(3)?;
        let doc_id = decode_catalog_id("posting document", stored_doc_id)?;
        let positions = decode_legacy_positions(&row.get::<_, Vec<u8>>(4)?)?;
        let stored_length = row.get::<_, Option<i64>>(5)?.ok_or_else(|| {
            SQLiteError::StorageBackend(format!(
                "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing document length"
            ))
        })?;
        let doc_length = decode_catalog_id("posting document length", stored_length)?;
        let next_cluster = cluster_id(doc_id);
        let same_group = group.as_ref().is_some_and(
            |(group_table, group_field, group_term, group_cluster, _)| {
                group_table == &table
                    && group_field == &field
                    && group_term == &term
                    && *group_cluster == next_cluster
            },
        );
        if !same_group {
            if let Some(cluster) = group.take() {
                insert_migrated_cluster(tx, cluster)?;
            }
            group = Some((table, field, term, next_cluster, Vec::new()));
        }
        group
            .as_mut()
            .expect("migration group exists")
            .4
            .push(ClusterPosting {
                doc_id,
                term_freq: positions.len() as u64,
                doc_length,
                positions,
            });
    }
    if let Some(cluster) = group {
        insert_migrated_cluster(tx, cluster)?;
    }
    Ok(())
}

fn migrate_legacy_document_terms_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT table_name, doc_id, field, term
           FROM _postings
          ORDER BY table_name, doc_id, field, term",
    )?;
    let mut rows = statement.query([])?;
    let mut group: Option<(String, i64, String, Vec<String>)> = None;
    while let Some(row) = rows.next()? {
        let table = row.get::<_, String>(0)?;
        let doc_id = row.get::<_, i64>(1)?;
        decode_catalog_id("posting document", doc_id)?;
        let field = row.get::<_, String>(2)?;
        let term = row.get::<_, String>(3)?;
        let same_group =
            group
                .as_ref()
                .is_some_and(|(group_table, group_doc_id, group_field, _)| {
                    group_table == &table && *group_doc_id == doc_id && group_field == &field
                });
        if !same_group {
            if let Some(document) = group.take() {
                insert_migrated_document_terms(tx, document)?;
            }
            group = Some((table, doc_id, field, Vec::new()));
        }
        group
            .as_mut()
            .expect("migration document group exists")
            .3
            .push(term);
    }
    if let Some(document) = group {
        insert_migrated_document_terms(tx, document)?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct LegacySequenceState {
    start: i64,
    increment: i64,
    current: i64,
}

pub(super) fn migration_relation(value: &str) -> Result<RelationIdentity> {
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

    pub(super) fn run_migrations(&self) -> Result<bool> {
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
                    } else if *version == 20 {
                        Self::migrate_legacy_hnsw_aliases_v20(&tx)?;
                    } else if *version == 22 {
                        Self::migrate_clustered_postings_v22(&tx)?;
                    } else if *version == 23 {
                        Self::migrate_sequence_persistence_v23(&tx, sql)?;
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

    fn migrate_sequence_persistence_v23(
        tx: &rusqlite::Transaction<'_>,
        data_migration_sql: &str,
    ) -> Result<()> {
        let persistence_already_present = Self::table_columns(tx, "_sequences")?
            .is_some_and(|columns| columns.contains_key("persistence"));
        if !persistence_already_present {
            tx.execute_batch(
                "ALTER TABLE _sequences
                    ADD COLUMN persistence TEXT NOT NULL DEFAULT 'p'
                    CHECK (persistence IN ('p', 'u'));",
            )?;
        }
        Ok(tx.execute_batch(data_migration_sql)?)
    }

    pub(super) fn migrate_relation_namespace_v17(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let migrations = collect_sqlite_relation_migrations(tx)?;
        create_structural_relation_tables(tx)?;
        insert_relation_parents(tx, &migrations.seen)?;
        insert_relation_children(tx, &migrations)?;
        finish_sqlite_relation_migration(tx)
    }

    fn clustered_posting_tables_have_current_shape(conn: &rusqlite::Connection) -> Result<bool> {
        let posting_clusters = Self::table_columns(conn, "_posting_clusters")?;
        let posting_documents = Self::table_columns(conn, "_posting_documents")?;
        let posting_clusters_ok = posting_clusters.as_ref().is_some_and(|cols| {
            [
                ("table_name", "TEXT"),
                ("field", "TEXT"),
                ("term", "TEXT"),
                ("cluster_id", "INTEGER"),
                ("posting_count", "INTEGER"),
                ("score_blob", "BLOB"),
                ("positions_blob", "BLOB"),
            ]
            .into_iter()
            .all(|(column, expected)| {
                cols.get(column)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            })
        });
        let posting_documents_ok = posting_documents.as_ref().is_some_and(|cols| {
            [
                ("table_name", "TEXT"),
                ("doc_id", "INTEGER"),
                ("field", "TEXT"),
                ("terms_blob", "BLOB"),
            ]
            .into_iter()
            .all(|(column, expected)| {
                cols.get(column)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            })
        });
        Ok(posting_clusters_ok && posting_documents_ok)
    }

    fn migrate_clustered_postings_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let legacy_postings_exist = table_exists(tx, "_postings")?;
        if !legacy_postings_exist && Self::clustered_posting_tables_have_current_shape(tx)? {
            return Ok(());
        }

        tx.execute_batch(
            "
            DROP TABLE IF EXISTS _posting_clusters_v22;
            DROP TABLE IF EXISTS _posting_documents_v22;

            CREATE TABLE _posting_clusters_v22 (
                table_name    TEXT NOT NULL,
                field         TEXT NOT NULL,
                term          TEXT NOT NULL,
                cluster_id    INTEGER NOT NULL,
                posting_count INTEGER NOT NULL CHECK (posting_count > 0),
                score_blob    BLOB NOT NULL,
                positions_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, field, term, cluster_id)
            ) WITHOUT ROWID;

            CREATE TABLE _posting_documents_v22 (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                field      TEXT NOT NULL,
                terms_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            ) WITHOUT ROWID;
            ",
        )?;

        if legacy_postings_exist {
            migrate_legacy_clusters_v22(tx)?;
            migrate_legacy_document_terms_v22(tx)?;
        }

        tx.execute_batch(
            "
            DROP TABLE IF EXISTS _postings;
            DROP TABLE IF EXISTS _posting_clusters;
            DROP TABLE IF EXISTS _posting_documents;
            ALTER TABLE _posting_clusters_v22 RENAME TO _posting_clusters;
            ALTER TABLE _posting_documents_v22 RENAME TO _posting_documents;
            ",
        )?;
        Ok(())
    }

    /// Correct historical `hnsw` catalog rows whose durable implementation is
    /// IVF. HNSW used to be a SQL alias for IVF, and a few releases persisted
    /// the requested spelling rather than the physical index kind. Treating
    /// those rows as native HNSW after v19 makes engine reopen fail because no
    /// `_hnsw_indexes` row can exist for them.
    ///
    /// Physical metadata is the source of truth: rewrite only when every
    /// indexed column has IVF metadata and none has HNSW metadata. Genuine
    /// persistent HNSW indexes are therefore left untouched.
    fn migrate_legacy_hnsw_aliases_v20(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        if !table_exists(tx, "_catalog_indexes")?
            || !table_exists(tx, "_ivf_indexes")?
            || !table_exists(tx, "_hnsw_indexes")?
        {
            return Ok(());
        }
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT name, table_name, columns
                   FROM _catalog_indexes
                  WHERE lower(index_type) = 'hnsw'
                  ORDER BY name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut legacy_aliases = Vec::new();
        for (name, table_name, columns_json) in candidates {
            let columns: Vec<String> = serde_json::from_str(&columns_json)?;
            if columns.is_empty() {
                continue;
            }
            let mut all_ivf = true;
            let mut any_hnsw = false;
            for field in columns {
                let has_ivf = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM _ivf_indexes
                          WHERE table_name = ?1 AND field = ?2
                     )",
                    params![table_name, field],
                    |row| row.get::<_, bool>(0),
                )?;
                let has_hnsw = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM _hnsw_indexes
                          WHERE table_name = ?1 AND field = ?2
                     )",
                    params![table_name, field],
                    |row| row.get::<_, bool>(0),
                )?;
                all_ivf &= has_ivf;
                any_hnsw |= has_hnsw;
            }
            if all_ivf && !any_hnsw {
                legacy_aliases.push(name);
            }
        }

        for name in legacy_aliases {
            tx.execute(
                "UPDATE _catalog_indexes
                    SET index_type = 'ivf'
                  WHERE name = ?1 AND lower(index_type) = 'hnsw'",
                params![name],
            )?;
        }
        Ok(())
    }

    pub(super) fn claim_relation(
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

    pub(super) fn release_relation(
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

    pub(super) fn ensure_fts_storage_shape(conn: &rusqlite::Connection) -> Result<bool> {
        let doc_lengths = Self::table_columns(conn, "_doc_lengths")?;
        let doc_lengths_ok = doc_lengths
            .as_ref()
            .is_some_and(|cols| cols.contains_key("field") && cols.contains_key("length"));
        if doc_lengths_ok && Self::clustered_posting_tables_have_current_shape(conn)? {
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
            DROP TABLE IF EXISTS _posting_clusters;
            DROP TABLE IF EXISTS _posting_documents;
            DROP TABLE IF EXISTS _doc_lengths;
            DROP TABLE IF EXISTS _field_stats;

            CREATE TABLE _posting_clusters (
                table_name     TEXT NOT NULL,
                field          TEXT NOT NULL,
                term           TEXT NOT NULL,
                cluster_id     INTEGER NOT NULL,
                posting_count  INTEGER NOT NULL CHECK (posting_count > 0),
                score_blob     BLOB NOT NULL,
                positions_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, field, term, cluster_id)
            ) WITHOUT ROWID;

            CREATE TABLE _posting_documents (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                field      TEXT NOT NULL,
                terms_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            ) WITHOUT ROWID;

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

    pub(super) fn table_columns(
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

    pub(super) fn ensure_column_stats_shape(conn: &rusqlite::Connection) -> Result<()> {
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
}
