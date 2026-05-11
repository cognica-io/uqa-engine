//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Run with: cargo run -p uqa-engine --example compressed_encrypted_catalog

use std::error::Error;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::SQLiteCompressionOptions;

fn main() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("compressed-encrypted.uqac.sqlite3");
    let key = "correct horse battery staple";
    let compression = SQLiteCompressionOptions::default();

    {
        let engine = Engine::open_compressed_encrypted(&path, key, compression)?;
        engine.sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
            &[],
        )?;
        engine.sql(
            "INSERT INTO notes (id, title, body) VALUES \
             (1, 'private note', 'this catalog is compressed before encryption')",
            &[],
        )?;
    }

    let reopened = Engine::open_compressed_encrypted(&path, key, compression)?;
    let result = reopened.sql("SELECT title FROM notes WHERE id = 1", &[])?;
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("private note".to_string()))
    );
    if Engine::open_compressed_encrypted(&path, "wrong key", compression).is_ok() {
        return Err("wrong key unexpectedly opened compressed encrypted catalog".into());
    }
    if Engine::open(&path).is_ok() {
        return Err("plaintext SQLite unexpectedly opened compressed container".into());
    }

    println!(
        "Compressed encrypted catalog reopened successfully at {}",
        path.display()
    );
    println!("Wrong-key and plaintext opens failed as expected");
    Ok(())
}
