//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable storage on the pure-Rust redb backend, plus the transaction and
//! savepoint semantics the engine guarantees on top of it.
//!
//! Run with: cargo run -p example-storage-transactions

use std::path::{Path, PathBuf};
use std::sync::Arc;

use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = scratch_path();
    remove_database(&path);

    // Pass 1: create the schema and commit rows, then drop the engine so
    // everything must survive on disk rather than in process memory.
    {
        let engine = open(&path)?;
        engine.sql(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT, balance INTEGER)",
            &[],
        )?;
        engine.sql(
            "INSERT INTO accounts (id, owner, balance) VALUES \
             (1, 'alice', 100), (2, 'bob', 50)",
            &[],
        )?;
        println!("pass 1 wrote {} accounts", count(&engine, "accounts")?);
    }

    // Pass 2: reopen the same file. redb is a single-file store, so this is
    // the whole durability story: no export, no migration step.
    let engine = open(&path)?;
    println!(
        "pass 2 reopened and found {} accounts",
        count(&engine, "accounts")?
    );

    // An explicit transaction that is rolled back leaves no trace.
    engine.sql("BEGIN", &[])?;
    engine.sql("UPDATE accounts SET balance = 0", &[])?;
    println!(
        "  inside transaction, alice = {}",
        balance(&engine, "alice")?
    );
    engine.sql("ROLLBACK", &[])?;
    println!(
        "  after ROLLBACK,    alice = {}",
        balance(&engine, "alice")?
    );

    // Savepoints are nested markers inside one transaction. Rolling back to a
    // savepoint undoes the work after it while keeping the work before it, and
    // the enclosing transaction stays open and atomic throughout.
    engine.sql("BEGIN", &[])?;
    engine.sql(
        "UPDATE accounts SET balance = balance - 10 WHERE owner = 'alice'",
        &[],
    )?;
    engine.sql("SAVEPOINT after_debit", &[])?;
    engine.sql(
        "UPDATE accounts SET balance = balance - 90 WHERE owner = 'alice'",
        &[],
    )?;
    println!(
        "\n  after both debits,       alice = {}",
        balance(&engine, "alice")?
    );
    engine.sql("ROLLBACK TO SAVEPOINT after_debit", &[])?;
    println!(
        "  rolled back to savepoint, alice = {}",
        balance(&engine, "alice")?
    );
    engine.sql("RELEASE SAVEPOINT after_debit", &[])?;
    engine.sql("COMMIT", &[])?;
    println!(
        "  committed,               alice = {}",
        balance(&engine, "alice")?
    );

    // A second session over the same provider sees committed writes. Sessions
    // are isolated for uncommitted state but share the durable database.
    let second = engine.new_session()?;
    println!(
        "\nsecond session sees alice = {}",
        balance(&second, "alice")?
    );

    drop(second);
    drop(engine);
    remove_database(&path);
    Ok(())
}

fn open(path: &Path) -> Result<Engine, Box<dyn std::error::Error>> {
    let provider: Arc<dyn PersistentStorageProvider> = Arc::new(RedbStorage::open(path)?);
    Ok(Engine::from_persistent_provider(provider)?)
}

fn count(engine: &Engine, table: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let result = engine.sql(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])?;
    Ok(integer(result.rows.first().and_then(|row| row.get("n"))))
}

fn balance(engine: &Engine, owner: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let result = engine.sql(
        &format!("SELECT balance FROM accounts WHERE owner = '{owner}'"),
        &[],
    )?;
    Ok(integer(
        result.rows.first().and_then(|row| row.get("balance")),
    ))
}

fn integer(value: Option<&uqa_core::Value>) -> i64 {
    match value {
        Some(uqa_core::Value::Int(n)) => *n,
        _ => -1,
    }
}

/// A process-unique path under the system temp directory so repeated runs and
/// concurrent runs do not collide.
fn scratch_path() -> PathBuf {
    std::env::temp_dir().join(format!("uqa-example-storage-{}.redb", std::process::id()))
}

fn remove_database(path: &Path) {
    let _ = std::fs::remove_file(path);
}
