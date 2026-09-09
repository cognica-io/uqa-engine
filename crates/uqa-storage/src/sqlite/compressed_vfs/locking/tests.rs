//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::process::Command;

use super::*;

#[test]
fn hot_journal_recovery_takes_exclusive_without_a_writer_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hot-journal.sqlite3");
    let mut recovery = FileLocks::open(&path).unwrap();
    let observer = FileLocks::open(&path).unwrap();
    assert!(recovery.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(recovery.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    assert!(!observer.check_reserved().unwrap());
    recovery.unlock(SQLITE_LOCK_SHARED).unwrap();
    assert_eq!(recovery.level(), SQLITE_LOCK_SHARED);
    assert!(!recovery.check_reserved().unwrap());
    recovery.unlock(SQLITE_LOCK_NONE).unwrap();
    recovery.unlock(SQLITE_LOCK_NONE).unwrap();
}

#[test]
fn rollback_releases_a_pending_writer_and_its_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rollback-pending.sqlite3");
    let mut reader = FileLocks::open(&path).unwrap();
    let mut writer = FileLocks::open(&path).unwrap();
    let mut replacement = FileLocks::open(&path).unwrap();
    assert!(reader.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(writer.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(writer.lock(SQLITE_LOCK_RESERVED).unwrap());
    assert!(!writer.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    assert!(!replacement.lock(SQLITE_LOCK_SHARED).unwrap());
    writer.unlock(SQLITE_LOCK_SHARED).unwrap();
    assert!(replacement.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(replacement.lock(SQLITE_LOCK_RESERVED).unwrap());
    assert!(!replacement.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    reader.unlock(SQLITE_LOCK_NONE).unwrap();
    writer.unlock(SQLITE_LOCK_NONE).unwrap();
    assert!(replacement.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    replacement.unlock(SQLITE_LOCK_NONE).unwrap();
}

#[test]
fn lock_protocol_survives_process_boundaries() {
    if let Some(path) = std::env::var_os("UQA_VFS_LOCK_PROBE_PATH") {
        probe_locks(Path::new(&path));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("process-locks.sqlite3");
    let mut reader = FileLocks::open(&path).unwrap();
    let mut writer = FileLocks::open(&path).unwrap();
    assert!(reader.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(writer.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(writer.lock(SQLITE_LOCK_RESERVED).unwrap());
    run_probe(&path, "reserved");
    assert!(!writer.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    assert_eq!(writer.level(), SQLITE_LOCK_PENDING);
    run_probe(&path, "pending");
    reader.unlock(SQLITE_LOCK_NONE).unwrap();
    assert!(writer.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    run_probe(&path, "exclusive");
    writer.unlock(SQLITE_LOCK_SHARED).unwrap();
    run_probe(&path, "shared");
    writer.unlock(SQLITE_LOCK_NONE).unwrap();
    run_probe(&path, "released");
}

fn run_probe(path: &Path, expected: &str) {
    let (_, test_name) = concat!(
        module_path!(),
        "::lock_protocol_survives_process_boundaries"
    )
    .split_once("::")
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env("UQA_VFS_LOCK_PROBE_PATH", path)
        .env("UQA_VFS_LOCK_PROBE_STATE", expected)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{expected}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A misspelled test filter would otherwise silently report success without executing the child assertions.
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn probe_locks(path: &Path) {
    let expected = std::env::var("UQA_VFS_LOCK_PROBE_STATE").unwrap();
    let mut locks = FileLocks::open(path).unwrap();
    let reserved = matches!(expected.as_str(), "reserved" | "pending" | "exclusive");
    let readable = matches!(expected.as_str(), "reserved" | "shared" | "released");
    assert_eq!(locks.check_reserved().unwrap(), reserved);
    assert_eq!(locks.lock(SQLITE_LOCK_SHARED).unwrap(), readable);
    if readable {
        assert_eq!(locks.lock(SQLITE_LOCK_RESERVED).unwrap(), !reserved);
    }
    if expected == "released" {
        assert!(locks.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
        assert!(locks.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
        locks.unlock(SQLITE_LOCK_SHARED).unwrap();
        locks.unlock(SQLITE_LOCK_SHARED).unwrap();
    }
    locks.unlock(SQLITE_LOCK_NONE).unwrap();
    locks.unlock(SQLITE_LOCK_NONE).unwrap();
}
