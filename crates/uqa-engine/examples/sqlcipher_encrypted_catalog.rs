//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
// Run with: cargo run -p uqa-engine --example sqlcipher_encrypted_catalog

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("catalog.sqlite3");
    let key = "demo encryption key";

    {
        let engine = Engine::open_encrypted(&path, key)?;
        engine.sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
            &[],
        )?;
        engine.sql(
            "INSERT INTO notes (id, title, body) VALUES \
             (1, 'private note', 'this catalog is encrypted with SQLCipher')",
            &[],
        )?;
    }

    let reopened = Engine::open_encrypted(&path, key)?;
    let result = reopened.sql("SELECT title, body FROM notes WHERE id = 1", &[])?;
    let row = result
        .rows
        .first()
        .ok_or_else(|| std::io::Error::other("missing row after encrypted reopen"))?;
    assert_eq!(
        row.get("title"),
        Some(&Value::Str("private note".to_string()))
    );

    if Engine::open_encrypted(&path, "wrong key").is_ok() {
        return Err(std::io::Error::other("wrong key opened encrypted catalog").into());
    }
    if Engine::open(&path).is_ok() {
        return Err(std::io::Error::other("plaintext open read encrypted catalog").into());
    }

    println!(
        "Encrypted catalog reopened successfully at {}",
        path.display()
    );
    println!("Wrong-key and plaintext opens failed as expected");
    Ok(())
}
