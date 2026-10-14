//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog lifetime is independent of the optional foreign source handlers.

use super::columns::open;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_definitions_survive_without_handlers(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-definitions.db");
    let engine = open(provider, &path);
    for wrapper in ["memory_fdw", "arrow_fdw", "duckdb_fdw"] {
        engine
            .sql(
                &format!(
                    "CREATE SERVER {wrapper}_source FOREIGN DATA WRAPPER {wrapper};
                     CREATE FOREIGN TABLE {wrapper}_rows(id integer, value text DEFAULT 'kept') SERVER {wrapper}_source"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(
                &format!("BEGIN; ALTER FOREIGN TABLE {wrapper}_rows DROP value; ROLLBACK"),
                &[],
            )
            .unwrap();
    }
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    for wrapper in ["memory_fdw", "arrow_fdw", "duckdb_fdw"] {
        let name = format!("{wrapper}_rows");
        let table = engine.foreign_table_definition(&name).unwrap().unwrap();
        assert_eq!(table.columns.len(), 2);
        assert!(table.columns[1].default.is_some());
        assert_eq!(
            engine
                .foreign_server_definition(&table.server_name)
                .unwrap()
                .unwrap()
                .fdw_type,
            wrapper
        );
        for sql in [
            format!("SELECT * FROM {name}"),
            format!("SELECT a.id FROM {name} a JOIN {name} b ON a.id=b.id"),
        ] {
            let error = engine.sql(&sql, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                "FDW support is not available in this build"
            );
        }
        engine
            .sql(&format!("DROP SERVER {wrapper}_source CASCADE"), &[])
            .unwrap();
        assert!(engine.foreign_table_definition(&name).unwrap().is_none());
    }
}
