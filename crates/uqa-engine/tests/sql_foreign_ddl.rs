//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE SERVER` + `CREATE FOREIGN TABLE` DDL plumbing.

use uqa_engine::Engine;

#[test]
fn create_server_then_create_foreign_table() {
    let eng = Engine::new();
    eng.sql(
        "CREATE SERVER s1 FOREIGN DATA WRAPPER duckdb_fdw \
         OPTIONS (database 'sample.db')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE remote_books (id INTEGER, title TEXT) \
         SERVER s1 OPTIONS (source 'books.parquet')",
        &[],
    )
    .unwrap();
    let server = eng.foreign_server("s1").expect("server registered");
    assert_eq!(server.fdw_type, "duckdb_fdw");
    assert_eq!(server.options.get("database").unwrap(), "sample.db");
    let table = eng.foreign_table("remote_books").expect("foreign table");
    assert_eq!(table.server_name, "s1");
    assert_eq!(table.options.get("source").unwrap(), "books.parquet");
    assert_eq!(table.columns.len(), 2);
}

#[test]
fn unsupported_fdw_type_rejected() {
    let eng = Engine::new();
    let err = eng
        .sql(
            "CREATE SERVER bad FOREIGN DATA WRAPPER mongo_fdw OPTIONS (host 'a')",
            &[],
        )
        .unwrap_err();
    assert!(format!("{err:?}").contains("Unsupported FDW type"));
}
