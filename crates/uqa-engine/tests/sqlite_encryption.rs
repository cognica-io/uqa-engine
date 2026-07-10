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

#[test]
fn open_auto_routes_by_detected_format() {
    use uqa_engine::{DatabaseFileFormat, SQLiteCompressionOptions, SQLiteError};

    let dir = tempdir().unwrap();

    // Missing + key: creates a SQLCipher database.
    let enc_path = dir.path().join("auto-enc.sqlite3");
    {
        let eng = Engine::open_auto(&enc_path, Some("k1")).unwrap();
        eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
            .unwrap();
        eng.sql("INSERT INTO t (id, v) VALUES (1, 'x')", &[])
            .unwrap();
    }
    assert_eq!(
        Engine::detect_database_file(&enc_path).unwrap(),
        DatabaseFileFormat::Unrecognized
    );
    // Reopen with the key; without a key the error is EncryptionKeyRequired.
    {
        let eng = Engine::open_auto(&enc_path, Some("k1")).unwrap();
        let result = eng.sql("SELECT v FROM t", &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
    }
    assert!(matches!(
        Engine::open_auto(&enc_path, None),
        Err(SQLiteError::EncryptionKeyRequired)
    ));

    // Missing + no key: creates plaintext; a key on plaintext is NotEncrypted.
    let plain_path = dir.path().join("auto-plain.sqlite3");
    {
        let eng = Engine::open_auto(&plain_path, None).unwrap();
        eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
    }
    assert_eq!(
        Engine::detect_database_file(&plain_path).unwrap(),
        DatabaseFileFormat::PlainSQLite
    );
    assert!(Engine::open_auto(&plain_path, None).is_ok());
    assert!(matches!(
        Engine::open_auto(&plain_path, Some("k")),
        Err(SQLiteError::NotEncrypted)
    ));

    // Compressed containers round-trip through open_auto by header.
    let cc_path = dir.path().join("auto-cc.db");
    {
        let eng = Engine::open_compressed(&cc_path, SQLiteCompressionOptions::default()).unwrap();
        eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
    }
    assert_eq!(
        Engine::detect_database_file(&cc_path).unwrap(),
        DatabaseFileFormat::CompressedContainer { encrypted: false }
    );
    assert!(Engine::open_auto(&cc_path, None).is_ok());
    assert!(matches!(
        Engine::open_auto(&cc_path, Some("k")),
        Err(SQLiteError::NotEncrypted)
    ));

    let cce_path = dir.path().join("auto-cce.db");
    {
        let eng =
            Engine::open_compressed_encrypted(&cce_path, "k2", SQLiteCompressionOptions::default())
                .unwrap();
        eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
            .unwrap();
        eng.sql("INSERT INTO t (id, v) VALUES (1, 'c')", &[])
            .unwrap();
    }
    assert_eq!(
        Engine::detect_database_file(&cce_path).unwrap(),
        DatabaseFileFormat::CompressedContainer { encrypted: true }
    );
    assert!(matches!(
        Engine::open_auto(&cce_path, None),
        Err(SQLiteError::EncryptionKeyRequired)
    ));
    {
        let eng = Engine::open_auto(&cce_path, Some("k2")).unwrap();
        let result = eng.sql("SELECT v FROM t", &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
    }

    // Empty key is rejected up front.
    assert!(matches!(
        Engine::open_auto(&enc_path, Some("")),
        Err(SQLiteError::EmptyEncryptionKey)
    ));
}
