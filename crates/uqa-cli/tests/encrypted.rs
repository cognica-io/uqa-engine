//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encrypted-database coverage for the `usql` binary: `--key`,
//! `--key-file`, the `UQA_KEY` environment variable, format detection
//! for compressed containers, and the non-interactive failure modes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use uqa_engine::{Engine, SQLiteCompressionOptions};

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_usql"))
}

fn run_usql_env(args: &[&str], input: &str, history_dir: &Path, envs: &[(&str, &str)]) -> Output {
    let history = history_dir.join("hist");
    let mut command = Command::new(binary_path());
    command
        .args(args)
        .env("UQA_HISTORY", history)
        .env_remove("UQA_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("spawn usql");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn run_usql(args: &[&str], input: &str, history_dir: &Path) -> Output {
    run_usql_env(args, input, history_dir, &[])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn seed_encrypted_database(path: &Path, key: &str) {
    let engine = Engine::open_encrypted(path, key).expect("create encrypted db");
    engine
        .sql(
            "CREATE TABLE secrets (id INTEGER PRIMARY KEY, note TEXT)",
            &[],
        )
        .expect("create table");
    engine
        .sql(
            "INSERT INTO secrets (id, note) VALUES (1, 'classified')",
            &[],
        )
        .expect("insert");
}

fn seed_compressed_encrypted_database(path: &Path, key: &str) {
    let engine = Engine::open_compressed_encrypted(path, key, SQLiteCompressionOptions::default())
        .expect("create compressed encrypted db");
    engine
        .sql(
            "CREATE TABLE secrets (id INTEGER PRIMARY KEY, note TEXT)",
            &[],
        )
        .expect("create table");
    engine
        .sql(
            "INSERT INTO secrets (id, note) VALUES (7, 'container')",
            &[],
        )
        .expect("insert");
}

#[test]
fn key_flag_opens_sqlcipher_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("enc.db");
    seed_encrypted_database(&db, "hunter2");

    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "hunter2"],
        "SELECT note FROM secrets ORDER BY id;\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("classified"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn key_file_flag_opens_sqlcipher_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("enc.db");
    seed_encrypted_database(&db, "hunter2");
    let key_file = dir.path().join("key.txt");
    std::fs::write(&key_file, "hunter2\n").expect("write key file");

    let output = run_usql(
        &[
            "--db",
            db.to_str().unwrap(),
            "--key-file",
            key_file.to_str().unwrap(),
        ],
        "SELECT note FROM secrets;\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("classified"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn environment_variable_supplies_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("enc.db");
    seed_encrypted_database(&db, "hunter2");

    let output = run_usql_env(
        &["--db", db.to_str().unwrap()],
        "SELECT note FROM secrets;\n\\q\n",
        dir.path(),
        &[("UQA_KEY", "hunter2")],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("classified"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn wrong_key_fails_with_actionable_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("enc.db");
    seed_encrypted_database(&db, "hunter2");

    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "wrong"],
        "SELECT 1;\n\\q\n",
        dir.path(),
    );
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("open failed"), "{err}");
    assert!(err.contains("wrong encryption key"), "{err}");
}

#[test]
fn missing_key_fails_without_terminal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("enc.db");
    seed_encrypted_database(&db, "hunter2");

    let output = run_usql(
        &["--db", db.to_str().unwrap()],
        "SELECT 1;\n\\q\n",
        dir.path(),
    );
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("requires an encryption key"), "{err}");
    assert!(err.contains("--key"), "{err}");
}

#[test]
fn key_on_plaintext_database_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plain.db");
    {
        let engine = Engine::open(&db).expect("create plain db");
        engine
            .sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
    }

    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "whatever"],
        "SELECT 1;\n\\q\n",
        dir.path(),
    );
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("not encrypted"), "{err}");
}

#[test]
fn key_flag_creates_new_encrypted_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("fresh.db");

    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "s3cret"],
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);\n\
         INSERT INTO t (id, v) VALUES (1, 'persisted');\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    // The new file must not be plaintext SQLite.
    let prefix = std::fs::read(&db).expect("read db file");
    assert!(prefix.len() >= 16);
    assert_ne!(&prefix[..16], b"SQLite format 3\0");

    // Reopen with the same key and read the row back.
    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "s3cret"],
        "SELECT v FROM t;\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("persisted"), "{}", stdout(&output));
}

#[test]
fn compressed_encrypted_container_opens_with_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("container.db");
    seed_compressed_encrypted_database(&db, "hunter2");

    let output = run_usql(
        &["--db", db.to_str().unwrap(), "--key", "hunter2"],
        "SELECT note FROM secrets;\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("container"), "{}", stdout(&output));
}

#[test]
fn compressed_plain_container_opens_without_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("container.db");
    {
        let engine = Engine::open_compressed(&db, SQLiteCompressionOptions::default())
            .expect("create compressed db");
        engine
            .sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
            .expect("create table");
        engine
            .sql("INSERT INTO t (id, v) VALUES (1, 'zipped')", &[])
            .expect("insert");
    }

    let output = run_usql(
        &["--db", db.to_str().unwrap()],
        "SELECT v FROM t;\n\\q\n",
        dir.path(),
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("zipped"), "{}", stdout(&output));
}

#[test]
fn key_and_key_file_are_mutually_exclusive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_usql(&["--key", "a", "--key-file", "b"], "\\q\n", dir.path());
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("mutually exclusive"),
        "{}",
        stderr(&output)
    );
}
