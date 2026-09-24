//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod column_acls;
mod foreign_table_ownership;
mod index_relations;
mod log_count;
mod schema_security;
mod schema_version;
mod sequences;
mod table_ownership;
mod tuple_metadata;
mod view_ownership;

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
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            object_id: [1; 16],
            storage_generation: [1; 16],
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
fn migration_24_adds_persistent_table_storage_generations() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_generation"),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            object_id: [7; 16],
            storage_generation: [7; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute("ALTER TABLE _tables DROP COLUMN storage_generation", [])?;
        connection.execute(
            "UPDATE _metadata SET value = '23' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc).unwrap();
    let mut schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.storage_generation, [0; 16]);
    schema.storage_generation = [9; 16];
    upgraded.save_table(&schema).unwrap();
    assert_eq!(
        upgraded.load_tables().unwrap()[0].storage_generation,
        [9; 16]
    );
}

#[test]
fn migration_24_preserves_a_storage_generation_installed_before_its_version_marker() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "early_generation"),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            object_id: [11; 16],
            storage_generation: [11; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute(
            "UPDATE _metadata SET value = '23' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc.clone()).unwrap();
    assert_eq!(
        upgraded.load_tables().unwrap()[0].storage_generation,
        [11; 16]
    );
    mc.with(|connection| {
        let version: String = connection.query_row(
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
fn migration_25_adds_persistent_table_object_identities() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_object"),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            object_id: [7; 16],
            storage_generation: [8; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute("ALTER TABLE _tables DROP COLUMN object_id", [])?;
        connection.execute(
            "UPDATE _metadata SET value = '24' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc).unwrap();
    let mut schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.object_id, [0; 16]);
    schema.object_id = [9; 16];
    upgraded.save_table(&schema).unwrap();
    assert_eq!(upgraded.load_tables().unwrap()[0].object_id, [9; 16]);
}

#[test]
fn migration_19_creates_complete_hnsw_storage_schema() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "DROP TABLE _hnsw_edges;
                 DROP TABLE _hnsw_nodes;
                 DROP TABLE _hnsw_indexes;
                 UPDATE _metadata SET value = '18' WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|conn| {
            for table in ["_hnsw_indexes", "_hnsw_nodes", "_hnsw_edges"] {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 1, "missing HNSW migration table {table}");
            }
            let mut statement = conn.prepare("PRAGMA table_info(_hnsw_indexes)")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
            assert!(columns.contains("revision"));
            assert!(columns.contains("format_version"));
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
fn migration_20_repairs_only_hnsw_rows_backed_by_legacy_ivf_metadata() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&empty_table("public", "legacy"))
        .unwrap();
    current
        .save_table(&empty_table("public", "native"))
        .unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "DROP TABLE _catalog_indexes;
                 CREATE TABLE _catalog_indexes (
                     name TEXT PRIMARY KEY,
                     index_type TEXT NOT NULL,
                     table_name TEXT NOT NULL,
                     columns TEXT NOT NULL,
                     parameters TEXT NOT NULL
                 );
                 INSERT INTO _catalog_indexes
                     (name, index_type, table_name, columns, parameters)
                 VALUES
                     ('legacy_idx', 'hnsw', 'public.legacy', '[\"embedding\"]', '{}'),
                     ('native_idx', 'hnsw', 'public.native', '[\"embedding\"]', '{}');
                 INSERT INTO _ivf_indexes
                     (table_name, field, dimensions, nlist, nprobe,
                      train_threshold, state, trained_size,
                      deletes_since_train, vector_count)
                 VALUES
                     ('public.legacy', 'embedding', 2, 100, 10,
                      256, 'untrained', 0, 0, 0);
                 INSERT INTO _hnsw_indexes
                     (table_name, field, dimensions, m, ef_construction,
                      ef_search, rebuild_threshold, seed, entry_node_id,
                      max_level, next_node_id, live_count, deleted_count,
                      revision, format_version)
                 VALUES
                     ('public.native', 'embedding', 2, 16, 200,
                      64, 1000, '7', NULL, 0, 0, 0, 0, 1, 1);
                 UPDATE _metadata SET value = '19'
                  WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    let rows = upgraded.load_catalog_indexes().unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.relation.name == "legacy_idx")
            .map(|row| row.index_type.as_str()),
        Some("ivf")
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.relation.name == "native_idx")
            .map(|row| row.index_type.as_str()),
        Some("hnsw")
    );
}

#[test]
fn migration_21_schedules_only_invalid_btree_fields_and_installs_guards() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "INSERT INTO _documents (table_name, doc_id, body)
                     VALUES
                     ('public.engine_meta', 1, '{\"key\":\"missing\"}'),
                     ('public.clean', 1, '{\"key\":\"kept\"}');
                 INSERT INTO _btree_indexes (table_name, field)
                     VALUES
                     ('public.engine_meta', 'key'),
                     ('public.clean', 'key');
                 INSERT INTO _btree_index_entries
                     (table_name, field, doc_id, value_json)
                     VALUES
                     ('public.clean', 'key', 1,
                      '{\"type\":\"Str\",\"value\":\"kept\"}');
                 DROP TRIGGER IF EXISTS _btree_documents_delete;
                 DROP TRIGGER IF EXISTS _btree_entries_document_insert;
                 DROP TRIGGER IF EXISTS _btree_entries_document_update;
                 DROP TRIGGER IF EXISTS _btree_documents_doc_id_update;
                 UPDATE _metadata SET value = '20'
                  WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|conn| {
            let definitions: i64 =
                conn.query_row("SELECT COUNT(*) FROM _btree_indexes", [], |row| row.get(0))?;
            let entries: i64 =
                conn.query_row("SELECT COUNT(*) FROM _btree_index_entries", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(definitions, 2);
            assert_eq!(entries, 1);
            let kept: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_indexes
                  WHERE table_name = 'public.clean' AND field = 'key'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(kept, 1);
            let pending: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_index_repairs
                  WHERE table_name = 'public.engine_meta' AND field = 'key'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(pending, 1);

            let guards: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'trigger' AND name IN (
                      '_btree_documents_delete',
                      '_btree_entries_document_insert',
                      '_btree_entries_document_update',
                      '_btree_documents_doc_id_update'
                  )",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(guards, 4);

            conn.pragma_update(None, "foreign_keys", "OFF")?;
            conn.execute(
                "DELETE FROM _documents
                  WHERE table_name = 'public.clean' AND doc_id = 1",
                [],
            )?;
            let cascaded: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_index_entries
                  WHERE table_name = 'public.clean' AND doc_id = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(cascaded, 0);
            Ok(())
        })
        .unwrap();
}
