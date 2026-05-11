//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compressed `SQLite` catalog open paths.

use uqa_engine::Engine;
use uqa_storage::SQLiteCompressionOptions;

fn exec(engine: &Engine, sql: &str) -> uqa_engine::SQLResult {
    engine.sql(sql, &[]).unwrap()
}

#[test]
fn compressed_engine_reopens_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine-compressed.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();

    {
        let engine = Engine::open_compressed(&path, options).unwrap();
        exec(
            &engine,
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT)",
        );
        exec(
            &engine,
            "INSERT INTO notes (id, title) VALUES (1, 'compressed catalog')",
        );
    }

    let engine = Engine::open_compressed(&path, options).unwrap();
    let rows = exec(&engine, "SELECT title FROM notes WHERE id = 1");
    assert_eq!(
        rows.rows[0].get("title"),
        Some(&uqa_core::Value::Str("compressed catalog".to_string()))
    );
    assert!(Engine::open(&path).is_err());
}

#[test]
fn lz4_compressed_engine_reopens_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine-compressed-lz4.uqac.sqlite3");
    let options = SQLiteCompressionOptions::lz4();

    {
        let engine = Engine::open_compressed(&path, options).unwrap();
        exec(
            &engine,
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT)",
        );
        exec(
            &engine,
            "INSERT INTO notes (id, title) VALUES (1, 'lz4 compressed catalog')",
        );
    }

    let engine = Engine::open_compressed(&path, options).unwrap();
    let rows = exec(&engine, "SELECT title FROM notes WHERE id = 1");
    assert_eq!(
        rows.rows[0].get("title"),
        Some(&uqa_core::Value::Str("lz4 compressed catalog".to_string()))
    );
}

#[test]
fn compressed_encrypted_engine_reopens_catalog_with_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine-compressed-encrypted.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let key = "correct horse battery staple";

    {
        let engine = Engine::open_compressed_encrypted(&path, key, options).unwrap();
        exec(
            &engine,
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT)",
        );
        exec(
            &engine,
            "INSERT INTO notes (id, title) VALUES (1, 'compressed encrypted catalog')",
        );
    }

    let engine = Engine::open_compressed_encrypted(&path, key, options).unwrap();
    let rows = exec(&engine, "SELECT title FROM notes WHERE id = 1");
    assert_eq!(
        rows.rows[0].get("title"),
        Some(&uqa_core::Value::Str(
            "compressed encrypted catalog".to_string()
        ))
    );
    assert!(Engine::open_compressed_encrypted(&path, "wrong key", options).is_err());
    assert!(Engine::open_compressed(&path, options).is_err());
    assert!(Engine::open(&path).is_err());
}
