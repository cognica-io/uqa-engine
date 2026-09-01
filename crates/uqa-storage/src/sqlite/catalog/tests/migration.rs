//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn prepare_legacy_v21_postings(mc: &ManagedConnection, include_length: bool) {
    mc.with(|conn| {
        conn.execute_batch(
            "DROP TABLE _posting_clusters;
             DROP TABLE _posting_documents;
             CREATE TABLE _postings (
                 table_name TEXT NOT NULL,
                 field      TEXT NOT NULL,
                 term       TEXT NOT NULL,
                 doc_id     INTEGER NOT NULL,
                 positions  BLOB NOT NULL,
                 PRIMARY KEY (table_name, field, term, doc_id)
             );
             CREATE INDEX _postings_doc_idx
                 ON _postings (table_name, doc_id);
             DELETE FROM _doc_lengths;
             DELETE FROM _field_stats;
             UPDATE _metadata SET value = '21' WHERE key = 'schema_version';",
        )?;
        if include_length {
            conn.execute(
                "INSERT INTO _doc_lengths(table_name, doc_id, field, length)
                 VALUES ('articles', 1, 'title', 3), ('articles', 2, 'title', 2)",
                [],
            )?;
            conn.execute(
                "INSERT INTO _field_stats(table_name, field, total_length)
                 VALUES ('articles', 'title', 5)",
                [],
            )?;
        }
        conn.execute(
            "INSERT INTO _postings(table_name, field, term, doc_id, positions)
             VALUES
                ('articles', 'title', 'rust', 1, X'0000000002000000'),
                ('articles', 'title', 'rust', 2, X'01000000'),
                ('articles', 'title', 'search', 2, X'00000000')",
            [],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn v22_migration_preserves_legacy_postings_in_clustered_rows() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(mc.clone()).unwrap();
    prepare_legacy_v21_postings(&mc, true);

    let _migrated = Catalog::open(mc.clone()).unwrap();
    mc.with(|conn| {
        let legacy_table_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = '_postings'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(legacy_table_count, 0);
        let (cluster_id, posting_count, score_blob, positions_blob): (i64, i64, Vec<u8>, Vec<u8>) =
            conn.query_row(
                "SELECT cluster_id, posting_count, score_blob, positions_blob
               FROM _posting_clusters
              WHERE table_name = 'articles' AND field = 'title' AND term = 'rust'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(cluster_id, 0);
        assert_eq!(posting_count, 2);
        let postings = crate::clustered_postings::decode_cluster(
            u64::try_from(cluster_id).unwrap(),
            &score_blob,
            &positions_blob,
        )
        .unwrap();
        assert_eq!(
            postings
                .iter()
                .map(|posting| (
                    posting.doc_id,
                    posting.term_freq,
                    posting.doc_length,
                    posting.positions.clone()
                ))
                .collect::<Vec<_>>(),
            vec![(1, 2, 3, vec![0, 2]), (2, 1, 2, vec![1])]
        );
        let terms_blob: Vec<u8> = conn.query_row(
            "SELECT terms_blob FROM _posting_documents
              WHERE table_name = 'articles' AND doc_id = 2 AND field = 'title'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            crate::clustered_postings::decode_terms(&terms_blob).unwrap(),
            vec!["rust".to_string(), "search".to_string()]
        );
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
fn v22_migration_is_idempotent_when_clustered_tables_precede_the_version_marker() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(mc.clone()).unwrap();
    let postings = vec![crate::clustered_postings::ClusterPosting {
        doc_id: 7,
        term_freq: 1,
        doc_length: 2,
        positions: vec![1],
    }];
    let (score_blob, positions_blob) =
        crate::clustered_postings::encode_cluster(&postings).unwrap();
    let terms_blob = crate::clustered_postings::encode_terms(&["rust".to_string()]).unwrap();
    mc.with(|conn| {
        conn.execute(
            "INSERT INTO _posting_clusters
                (table_name, field, term, cluster_id, posting_count,
                 score_blob, positions_blob)
             VALUES ('articles', 'title', 'rust', 0, 1, ?1, ?2)",
            params![score_blob, positions_blob],
        )?;
        conn.execute(
            "INSERT INTO _posting_documents(table_name, doc_id, field, terms_blob)
             VALUES ('articles', 7, 'title', ?1)",
            params![terms_blob],
        )?;
        conn.execute(
            "UPDATE _metadata SET value = '21' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let _migrated = Catalog::open(mc.clone()).unwrap();
    mc.with(|conn| {
        let stored: (i64, Vec<u8>) = conn.query_row(
            "SELECT posting_count, score_blob FROM _posting_clusters
              WHERE table_name = 'articles' AND field = 'title' AND term = 'rust'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored.0, 1);
        assert_eq!(
            crate::clustered_postings::decode_all_scores(0, &stored.1)
                .unwrap()
                .into_iter()
                .map(|posting| (posting.doc_id, posting.term_freq, posting.doc_length))
                .collect::<Vec<_>>(),
            vec![(7, 1, 2)]
        );
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
fn v22_migration_rolls_back_when_legacy_posting_has_no_length() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(mc.clone()).unwrap();
    prepare_legacy_v21_postings(&mc, false);

    let Err(error) = Catalog::open(mc.clone()) else {
        panic!("migration unexpectedly succeeded");
    };
    assert!(error.to_string().contains("missing document length"));
    mc.with(|conn| {
        let version: String = conn.query_row(
            "SELECT value FROM _metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, "21");
        let legacy_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM _postings", [], |row| row.get(0))?;
        assert_eq!(legacy_rows, 3);
        let temporary_tables: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'table'
                AND name IN ('_posting_clusters_v22', '_posting_documents_v22')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(temporary_tables, 0);
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
            object_id: [1; 16],
            storage_generation: [1; 16],
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
