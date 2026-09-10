//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Make indexes typed children of the shared relation namespace.

use super::super::super::{
    migration_relation, params, Catalog, OptionalExtension, RelationIdentity, Result, SQLiteError,
};

struct IndexMigration {
    relation: RelationIdentity,
    index_type: String,
    table: RelationIdentity,
    columns: String,
    parameters: String,
}

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let index_columns = Catalog::table_columns(tx, "_catalog_indexes")?;
    if index_columns
        .as_ref()
        .is_some_and(|columns| columns.contains_key("kind"))
    {
        return validate_structural_indexes(tx);
    }

    let indexes = load_indexes(tx, index_columns.as_ref())?;
    validate_index_migrations(tx, &indexes)?;
    create_structural_tables(tx)?;
    copy_existing_relations(tx)?;
    insert_indexes(tx, indexes)?;
    replace_relation_tables(tx)
}

fn load_indexes(
    tx: &rusqlite::Transaction<'_>,
    columns: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<Vec<IndexMigration>> {
    let Some(columns) = columns else {
        return Ok(Vec::new());
    };
    if columns.contains_key("schema_name") {
        let mut statement = tx.prepare(
            "SELECT schema_name, relation_name, index_type,
                    table_schema_name, table_relation_name, columns, parameters
               FROM _catalog_indexes ORDER BY schema_name, relation_name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(IndexMigration {
                relation: RelationIdentity::new(row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                index_type: row.get(2)?,
                table: RelationIdentity::new(row.get::<_, String>(3)?, row.get::<_, String>(4)?),
                columns: row.get(5)?,
                parameters: row.get(6)?,
            })
        })?;
        return rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(SQLiteError::from);
    }

    let mut statement = tx.prepare(
        "SELECT name, index_type, table_name, columns, parameters
           FROM _catalog_indexes ORDER BY name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut indexes = Vec::new();
    for row in rows {
        let (name, index_type, table_name, columns, parameters) = row?;
        let table = migration_relation(&table_name)?;
        if table.schema.starts_with("pg_temp_") {
            continue;
        }
        indexes.push(IndexMigration {
            relation: RelationIdentity::from_legacy_index_name(&name, &table),
            index_type,
            table,
            columns,
            parameters,
        });
    }
    Ok(indexes)
}

fn validate_index_migrations(
    tx: &rusqlite::Transaction<'_>,
    indexes: &[IndexMigration],
) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for index in indexes {
        if index.table.schema.starts_with("pg_temp_") {
            continue;
        }
        if index.relation.schema != index.table.schema {
            return Err(SQLiteError::StorageBackend(format!(
                "catalog index `{}` belongs to a different schema than table `{}`",
                index.relation.qualified_name(),
                index.table.qualified_name()
            )));
        }
        if !seen.insert(index.relation.clone()) {
            return Err(SQLiteError::StorageBackend(format!(
                "catalog index migration collision for `{}`",
                index.relation.qualified_name()
            )));
        }
        let table_exists = tx
            .query_row(
                "SELECT 1 FROM _tables WHERE schema_name = ?1 AND relation_name = ?2",
                params![index.table.schema, index.table.name],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !table_exists {
            return Err(SQLiteError::StorageBackend(format!(
                "catalog index `{}` references missing table `{}`",
                index.relation.qualified_name(),
                index.table.qualified_name()
            )));
        }
        let conflicting_kind = tx
            .query_row(
                "SELECT kind FROM _relations WHERE schema_name = ?1 AND relation_name = ?2",
                params![index.relation.schema, index.relation.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(kind) = conflicting_kind {
            return Err(SQLiteError::StorageBackend(format!(
                "catalog index `{}` conflicts with existing {kind}",
                index.relation.qualified_name()
            )));
        }
    }
    Ok(())
}

fn create_structural_tables(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE _relations_v34 (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL CHECK (
                kind IN ('table', 'view', 'sequence', 'foreign_table', 'index')
            ),
            PRIMARY KEY (schema_name, relation_name),
            UNIQUE (schema_name, relation_name, kind),
            FOREIGN KEY (schema_name) REFERENCES _schemas(name) ON DELETE RESTRICT
        );
        CREATE TABLE _tables_v34 (
            schema_name        TEXT NOT NULL,
            relation_name      TEXT NOT NULL,
            kind               TEXT NOT NULL DEFAULT 'table' CHECK (kind = 'table'),
            analyzer           TEXT NOT NULL,
            fts_fields         TEXT NOT NULL,
            vector_fields      TEXT NOT NULL,
            columns            TEXT,
            constraints        TEXT NOT NULL DEFAULT '',
            storage_generation BLOB NOT NULL DEFAULT X'00000000000000000000000000000000',
            object_id          BLOB NOT NULL DEFAULT X'00000000000000000000000000000000',
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations_v34(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _sequences_v34 (
            schema_name            TEXT NOT NULL,
            relation_name          TEXT NOT NULL,
            kind                   TEXT NOT NULL DEFAULT 'sequence' CHECK (kind = 'sequence'),
            start                  INTEGER NOT NULL,
            increment              INTEGER NOT NULL,
            current                INTEGER NOT NULL,
            called                 INTEGER NOT NULL DEFAULT 1 CHECK (called IN (0, 1)),
            persistence            TEXT NOT NULL DEFAULT 'p' CHECK (persistence IN ('p', 'u')),
            object_id              BLOB NOT NULL DEFAULT X'00000000000000000000000000000000',
            data_type              TEXT NOT NULL DEFAULT 'bigint',
            min_value              INTEGER NOT NULL DEFAULT 0,
            max_value              INTEGER NOT NULL DEFAULT 0,
            cycle                  INTEGER NOT NULL DEFAULT 0 CHECK (cycle IN (0, 1)),
            cache_size             INTEGER NOT NULL DEFAULT 1 CHECK (cache_size > 0),
            definition_generation BLOB NOT NULL DEFAULT X'',
            owner_table_object_id  BLOB,
            owner_column_object_id BLOB,
            owner_dependency       TEXT,
            role_owner             TEXT NOT NULL DEFAULT 'uqa',
            acl_json               TEXT,
            log_count              INTEGER NOT NULL DEFAULT 0 CHECK (log_count >= 0),
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations_v34(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _foreign_tables_v34 (
            schema_name   TEXT NOT NULL,
            relation_name TEXT NOT NULL,
            kind          TEXT NOT NULL DEFAULT 'foreign_table' CHECK (kind = 'foreign_table'),
            server_name   TEXT NOT NULL,
            columns_json  TEXT NOT NULL,
            options       TEXT NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations_v34(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _views_v34 (
            schema_name     TEXT NOT NULL,
            relation_name   TEXT NOT NULL,
            kind            TEXT NOT NULL DEFAULT 'view' CHECK (kind = 'view'),
            definition_json TEXT NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations_v34(schema_name, relation_name, kind)
                ON DELETE CASCADE
        );
        CREATE TABLE _catalog_indexes_v34 (
            schema_name         TEXT NOT NULL,
            relation_name       TEXT NOT NULL,
            kind                TEXT NOT NULL DEFAULT 'index' CHECK (kind = 'index'),
            index_type          TEXT NOT NULL,
            table_schema_name   TEXT NOT NULL,
            table_relation_name TEXT NOT NULL,
            columns             TEXT NOT NULL,
            parameters          TEXT NOT NULL,
            PRIMARY KEY (schema_name, relation_name),
            CHECK (schema_name = table_schema_name),
            FOREIGN KEY (schema_name, relation_name, kind)
                REFERENCES _relations_v34(schema_name, relation_name, kind)
                ON DELETE CASCADE,
            FOREIGN KEY (table_schema_name, table_relation_name)
                REFERENCES _tables_v34(schema_name, relation_name)
                ON UPDATE CASCADE ON DELETE CASCADE
        );",
    )?;
    Ok(())
}

fn copy_existing_relations(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "INSERT INTO _relations_v34(schema_name, relation_name, kind)
             SELECT schema_name, relation_name, kind FROM _relations;
         INSERT INTO _tables_v34(
             schema_name, relation_name, kind, analyzer, fts_fields, vector_fields,
             columns, constraints, storage_generation, object_id
         )
             SELECT schema_name, relation_name, kind, analyzer, fts_fields, vector_fields,
                    columns, constraints, storage_generation, object_id
               FROM _tables;
         INSERT INTO _sequences_v34(
             schema_name, relation_name, kind, start, increment, current, called,
             persistence, object_id, data_type, min_value, max_value, cycle, cache_size,
             definition_generation, owner_table_object_id, owner_column_object_id,
             owner_dependency, role_owner, acl_json, log_count
         )
             SELECT schema_name, relation_name, kind, start, increment, current, called,
                    persistence, object_id, data_type, min_value, max_value, cycle, cache_size,
                    definition_generation, owner_table_object_id, owner_column_object_id,
                    owner_dependency, role_owner, acl_json, log_count
               FROM _sequences;
         INSERT INTO _foreign_tables_v34(
             schema_name, relation_name, kind, server_name, columns_json, options
         )
             SELECT schema_name, relation_name, kind, server_name, columns_json, options
               FROM _foreign_tables;
         INSERT INTO _views_v34(schema_name, relation_name, kind, definition_json)
             SELECT schema_name, relation_name, kind, definition_json FROM _views;",
    )?;
    Ok(())
}

fn insert_indexes(tx: &rusqlite::Transaction<'_>, indexes: Vec<IndexMigration>) -> Result<()> {
    for index in indexes {
        if index.table.schema.starts_with("pg_temp_") {
            continue;
        }
        tx.execute(
            "INSERT INTO _relations_v34(schema_name, relation_name, kind)
             VALUES (?1, ?2, 'index')",
            params![index.relation.schema, index.relation.name],
        )?;
        tx.execute(
            "INSERT INTO _catalog_indexes_v34(
                schema_name, relation_name, kind, index_type, table_schema_name,
                table_relation_name, columns, parameters
             ) VALUES (?1, ?2, 'index', ?3, ?4, ?5, ?6, ?7)",
            params![
                index.relation.schema,
                index.relation.name,
                index.index_type,
                index.table.schema,
                index.table.name,
                index.columns,
                index.parameters
            ],
        )?;
    }
    Ok(())
}

fn replace_relation_tables(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS _catalog_indexes;
         DROP TABLE _views;
         DROP TABLE _foreign_tables;
         DROP TABLE _sequences;
         DROP TABLE _tables;
         DROP TABLE _relations;
         ALTER TABLE _relations_v34 RENAME TO _relations;
         ALTER TABLE _tables_v34 RENAME TO _tables;
         ALTER TABLE _sequences_v34 RENAME TO _sequences;
         ALTER TABLE _foreign_tables_v34 RENAME TO _foreign_tables;
         ALTER TABLE _views_v34 RENAME TO _views;
         ALTER TABLE _catalog_indexes_v34 RENAME TO _catalog_indexes;",
    )?;
    Ok(())
}

fn validate_structural_indexes(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let invalid: Option<(String, String)> = tx
        .query_row(
            "SELECT i.schema_name, i.relation_name
               FROM _catalog_indexes AS i
               LEFT JOIN _relations AS r
                 ON r.schema_name = i.schema_name
                AND r.relation_name = i.relation_name
                AND r.kind = 'index'
              WHERE r.schema_name IS NULL
              LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((schema, name)) = invalid {
        return Err(SQLiteError::StorageBackend(format!(
            "catalog index `{}` has no index relation parent",
            RelationIdentity::new(schema, name).qualified_name()
        )));
    }
    let orphan: Option<(String, String)> = tx
        .query_row(
            "SELECT r.schema_name, r.relation_name
               FROM _relations AS r
               LEFT JOIN _catalog_indexes AS i
                 ON i.schema_name = r.schema_name
                AND i.relation_name = r.relation_name
              WHERE r.kind = 'index' AND i.schema_name IS NULL
              LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((schema, name)) = orphan {
        return Err(SQLiteError::StorageBackend(format!(
            "index relation `{}` has no catalog child",
            RelationIdentity::new(schema, name).qualified_name()
        )));
    }
    Ok(())
}
