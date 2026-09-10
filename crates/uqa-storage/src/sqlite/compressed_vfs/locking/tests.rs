//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::process::Command;

#[test]
fn read_only_locks_do_not_create_paths_or_allow_sidecar_writes() {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing").join("read-only.sqlite3");
    assert!(FileLocks::open(&path, true).is_err());
    assert!(!path.parent().unwrap().exists());
    drop(FileLocks::open(&path, false).unwrap());
    let locks = FileLocks::open(&path, true).unwrap();
    for mut file in [&locks.shared, &locks.reserved, &locks.pending] {
        assert!(file.write_all(b"must not be written").is_err());
    }
}

use super::*;

#[test]
fn hot_journal_recovery_takes_exclusive_without_a_writer_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hot-journal.sqlite3");
    let mut recovery = FileLocks::open(&path, false).unwrap();
    let observer = FileLocks::open(&path, false).unwrap();
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
    let mut reader = FileLocks::open(&path, false).unwrap();
    let mut writer = FileLocks::open(&path, false).unwrap();
    let mut replacement = FileLocks::open(&path, false).unwrap();
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
    let mut reader = FileLocks::open(&path, false).unwrap();
    let mut writer = FileLocks::open(&path, false).unwrap();
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
    for read_only in ["false", "true"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("UQA_VFS_LOCK_PROBE_PATH", path)
            .env("UQA_VFS_LOCK_PROBE_STATE", expected)
            .env("UQA_VFS_LOCK_PROBE_READ_ONLY", read_only)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{expected}, read_only={read_only}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // A misspelled test filter would otherwise silently report success without executing the child assertions.
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

fn probe_locks(path: &Path) {
    let expected = std::env::var("UQA_VFS_LOCK_PROBE_STATE").unwrap();
    let read_only = std::env::var("UQA_VFS_LOCK_PROBE_READ_ONLY").unwrap() == "true";
    let mut locks = FileLocks::open(path, read_only).unwrap();
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
