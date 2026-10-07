//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy shared-table versions cannot join a live exact-identity coordinator.

use super::*;

fn rejects_peer(path: &std::path::Path, expected: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "row_locks::cross_process::file::row_claims::tests::peer::row_claim_peer",
            "--test-threads=1",
        ])
        .env("UQA_ROW_CLAIM_TEST_PATH", path)
        .env("UQA_ROW_CLAIM_EXPECT_OPEN_ERROR", expected)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn incompatible_live_claim_table_rejects_open_without_resetting_the_table() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("protocol.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    // The predecessor's process records and liveness bytes have the same layout.
    // Its live owner excludes a table reset even though entries have changed.
    super::super::super::write_all_at(&coordinator.claim_file, &1_u32.to_be_bytes(), 4).unwrap();
    rejects_peer(&path, "invalid row claim table header");
    let mut version = [0; 4];
    super::super::super::read_exact_at(&coordinator.claim_file, &mut version, 4).unwrap();
    assert_eq!(version, 1_u32.to_be_bytes());
    drop(coordinator);
    let reopened = FileLockCoordinator::open(&path).unwrap();
    super::super::super::read_exact_at(&reopened.claim_file, &mut version, 4).unwrap();
    assert_eq!(version, 2_u32.to_be_bytes());
}

#[test]
fn incompatible_live_relation_registry_rejects_open_and_cold_epoch_rebuilds_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let registry = rusqlite::Connection::open(&coordinator.relation_path).unwrap();
    registry.pragma_update(None, "user_version", 1).unwrap();
    rejects_peer(&path, "invalid relation registry version");
    assert_eq!(
        registry
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
    drop(registry);
    drop(coordinator);
    let reopened = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(reopened.relation_registry_counts(), (0, 0));
    let registry = rusqlite::Connection::open(&reopened.relation_path).unwrap();
    assert_eq!(
        registry
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
}
