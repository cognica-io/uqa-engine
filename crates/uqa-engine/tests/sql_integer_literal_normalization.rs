//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer columns must read back `Value::Int` regardless of whether a
//! row was written with a SQL numeric literal (which the parser lexes
//! as Decimal) or a bind parameter, in the same session and after a
//! persistent reopen.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn updated_at_variants(eng: &Engine) -> Vec<(String, Value)> {
    eng.sql("SELECT id, updated_at FROM pages ORDER BY id", &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            let id = match row.get("id") {
                Some(Value::Str(id)) => id.clone(),
                other => panic!("expected Str id, got {other:?}"),
            };
            (id, row.get("updated_at").cloned().unwrap())
        })
        .collect()
}

#[test]
fn integer_columns_normalize_literal_and_bind_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("normalize.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE pages (id TEXT PRIMARY KEY, updated_at BIGINT NOT NULL)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO pages (id, updated_at) VALUES ('literal', 1692224000000)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO pages (id, updated_at) VALUES ($1, $2)",
            &[
                SQLParam::scalar(Value::Str("bind".into())),
                SQLParam::scalar(Value::Int(1_699_913_600_000)),
            ],
        )
        .unwrap();
        for (id, value) in updated_at_variants(&eng) {
            assert!(
                matches!(value, Value::Int(_)),
                "row `{id}` must read back Int in the writing session, got {value:?}",
            );
        }
    }
    {
        let eng = Engine::open(&db).unwrap();
        for (id, value) in updated_at_variants(&eng) {
            assert!(
                matches!(value, Value::Int(_)),
                "row `{id}` must read back Int after reopen, got {value:?}",
            );
        }
    }
}

#[test]
fn integer_columns_normalize_literal_updates() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE pages (id TEXT PRIMARY KEY, updated_at BIGINT NOT NULL)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO pages (id, updated_at) VALUES ('row', 1)", &[])
        .unwrap();
    eng.sql(
        "UPDATE pages SET updated_at = 1692224000000 WHERE id = 'row'",
        &[],
    )
    .unwrap();
    let rows = eng
        .sql("SELECT updated_at FROM pages WHERE id = 'row'", &[])
        .unwrap()
        .rows;
    assert_eq!(
        rows[0].get("updated_at"),
        Some(&Value::Int(1_692_224_000_000))
    );
}
