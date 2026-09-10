//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn migration_34_derives_index_identity_from_its_table_schema_and_discards_temp_ghosts() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current.save_schema("app").unwrap();
    current.save_schema("archive").unwrap();
    current.save_table(&empty_table("app", "docs")).unwrap();
    current.save_table(&empty_table("archive", "docs")).unwrap();
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
                     ('shared_idx', 'btree', 'app.docs', '[\"id\"]', '{}'),
                     ('archive_idx', 'btree', 'archive.docs', '[\"id\"]', '{}'),
                     ('shared.dot', 'btree', 'app.docs', '[\"id\"]', '{}'),
                     ('temp_idx', 'btree', 'pg_temp_91.docs', '[\"id\"]', '{}');
                 UPDATE _metadata SET value = '33' WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    let rows = upgraded.load_catalog_indexes().unwrap();
    assert_eq!(
        rows.into_iter()
            .map(|row| (row.relation.qualified_name(), row.table_name))
            .collect::<Vec<_>>(),
        [
            ("app.\"shared.dot\"".into(), "app.docs".into()),
            ("app.shared_idx".into(), "app.docs".into()),
            ("archive.archive_idx".into(), "archive.docs".into()),
        ]
    );
    connection
        .with(|conn| {
            let version: String = conn.query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
            let mut statement = conn.prepare("PRAGMA table_info(_catalog_indexes)")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
            assert!(columns.contains("schema_name"));
            assert!(columns.contains("kind"));
            assert!(columns.contains("table_schema_name"));
            let index_parents: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _relations WHERE kind = 'index'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(index_parents, 3);
            let foreign_key_violations: i64 =
                conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(foreign_key_violations, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_34_rejects_shared_namespace_collisions_without_partial_rewrite() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current.save_schema("app").unwrap();
    current.save_table(&empty_table("app", "docs")).unwrap();
    current.save_table(&empty_table("app", "taken")).unwrap();
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
                 VALUES ('taken', 'btree', 'app.docs', '[\"id\"]', '{}');
                 UPDATE _metadata SET value = '33' WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let Err(error) = Catalog::open(connection.clone()) else {
        panic!("shared namespace collision must reject migration");
    };
    assert!(error.to_string().contains("conflicts with existing table"));
    connection
        .with(|conn| {
            let version: String = conn.query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, "33");
            let legacy_rows: i64 =
                conn.query_row("SELECT COUNT(*) FROM _catalog_indexes", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(legacy_rows, 1);
            let partial_tables: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name LIKE '%_v34'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(partial_tables, 0);
            Ok(())
        })
        .unwrap();
}
