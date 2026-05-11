//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLCipher-backed catalog open path.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn encrypted_engine_reopens_catalog_with_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("catalog.sqlite3");
    let key = "engine encryption key";

    {
        let eng = Engine::open_encrypted(&path, key).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, rank INTEGER)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, title, rank) VALUES (1, 'alpha', 10)",
            &[],
        )
        .unwrap();
    }

    let eng = Engine::open_encrypted(&path, key).unwrap();
    let result = eng
        .sql("SELECT title, rank FROM docs WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("alpha".into()))
    );
    assert_eq!(result.rows[0].get("rank"), Some(&Value::Int(10)));

    assert!(Engine::open_encrypted(&path, "wrong key").is_err());
    assert!(Engine::open(&path).is_err());
}
