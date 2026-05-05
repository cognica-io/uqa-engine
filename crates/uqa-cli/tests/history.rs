//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Drives the compiled `usql` binary via piped stdin and confirms the
//! `$UQA_HISTORY` file picks up executed statements.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary_path() -> PathBuf {
    // Cargo gives integration tests the path to the test binary
    // through `CARGO_BIN_EXE_<name>`; for binary crates the entry is
    // generated automatically.
    PathBuf::from(env!("CARGO_BIN_EXE_usql"))
}

#[test]
fn usql_persists_history_to_uqa_history_env_var() {
    let dir = tempfile::tempdir().expect("tempdir");
    let history = dir.path().join("hist");

    let mut child = Command::new(binary_path())
        .env("UQA_HISTORY", &history)
        .env_remove("HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn usql");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        let _ = stdin.write_all(
            b"\\new\nCREATE TABLE t (id INTEGER PRIMARY KEY);\nINSERT INTO t (id) VALUES (1);\n\\q\n",
        );
    }
    let status = child.wait().expect("wait");
    assert!(status.success(), "usql exited with {status:?}");

    let body = std::fs::read_to_string(&history).expect("history file written");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected two recorded statements: {body:?}");
    assert!(
        lines[0].starts_with("CREATE TABLE t"),
        "first history line: {:?}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("INSERT INTO t"),
        "second history line: {:?}",
        lines[1]
    );
}

#[test]
fn usql_history_skips_consecutive_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let history = dir.path().join("hist");

    let mut child = Command::new(binary_path())
        .env("UQA_HISTORY", &history)
        .env_remove("HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn usql");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        let _ = stdin.write_all(
            b"\\new\nCREATE TABLE t (id INTEGER PRIMARY KEY);\nCREATE TABLE t (id INTEGER PRIMARY KEY);\n\\q\n",
        );
    }
    let _ = child.wait();
    let body = std::fs::read_to_string(&history).expect("history file written");
    let count = body
        .lines()
        .filter(|l| l.starts_with("CREATE TABLE t"))
        .count();
    assert_eq!(count, 1, "duplicate not deduped: {body:?}");
}
