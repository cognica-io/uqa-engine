//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "INSERT INTO _catalog_indexes
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
            .find(|row| row.name == "legacy_idx")
            .map(|row| row.index_type.as_str()),
        Some("ivf")
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.name == "native_idx")
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
                 INSERT INTO _documents(table_name, doc_id, body)
                     VALUES ('docs', 1, '{}');
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
                "INSERT INTO _documents(table_name, doc_id, body)
                 VALUES ('public.docs', 1, '{}')",
                [],
            )?;
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
