//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod peer;

#[test]
fn native_temporary_dependency_admission_honors_cancellation_and_process_exit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("references.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(peer.request("admission 0 0"), "admitted");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        coordinator
            .retain_temporary_role(1, 20, &cancelled)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        coordinator
            .foreign_temporary_role_reference(20, &cancelled)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert!(coordinator.temporary_role_slots.lock().by_key.is_empty());
    peer.terminate();
    coordinator
        .retain_temporary_role(1, 20, &CancellationToken::new())
        .unwrap();
    coordinator.release_temporary_role(1, 20);
}

#[test]
fn native_references_survive_transaction_release_and_die_with_the_process() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("references.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    let cancel = CancellationToken::new();
    assert_eq!(peer.request("retain 1 20"), "retained");
    assert_eq!(peer.request("retain 2 20"), "retained");
    assert!(coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap());
    assert!(!coordinator
        .foreign_temporary_role_reference(21, &cancel)
        .unwrap());
    assert_eq!(peer.request("release 1 20"), "released");
    assert!(coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap());
    assert_eq!(peer.request("release 2 20"), "released");
    assert!(!coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap());
    assert_eq!(peer.request("retain 1 21"), "retained");
    peer.terminate();
    assert!(!coordinator
        .foreign_temporary_role_reference(21, &cancel)
        .unwrap());
    // Stale slot bytes do not survive the native lease, and the abandoned space is reusable.
    coordinator.retain_temporary_role(3, 22, &cancel).unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(peer.request("probe 0 21"), "false");
    assert_eq!(peer.request("probe 0 22"), "true");
    coordinator.release_temporary_role(3, 22);
    assert_eq!(peer.request("probe 0 22"), "false");
}

#[test]
fn native_reference_headers_and_live_slots_reject_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("references.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let cancel = CancellationToken::new();
    write_all_at(&coordinator.file, &[1], HEADER_BASE).unwrap();
    assert!(coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap_err()
        .to_string()
        .contains("truncated"));
    write_all_at(&coordinator.file, &[1; HEADER_SIZE as usize], HEADER_BASE).unwrap();
    assert!(coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap_err()
        .to_string()
        .contains("invalid"));
    write_all_at(&coordinator.file, &[0; HEADER_SIZE as usize], HEADER_BASE).unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(peer.request("retain 1 20"), "retained");
    write_all_at(&coordinator.file, &[0; 8], SLOT_BASE + 16).unwrap();
    assert!(coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap_err()
        .to_string()
        .contains("invalid live"));
    peer.terminate();
    assert!(!coordinator
        .foreign_temporary_role_reference(20, &cancel)
        .unwrap());
}

#[test]
fn temporary_reference_regions_are_bounded_and_disjoint_from_relation_locks() {
    assert!(LEASE_BASE + u64::from(SLOT_COUNT) < crate::row_locks::cross_process::RELATION_BASE);
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("references.db")).unwrap();
    let cancel = CancellationToken::new();
    for role in 1..=SLOT_COUNT {
        coordinator.retain_temporary_role(1, role, &cancel).unwrap();
    }
    assert_eq!(
        coordinator
            .retain_temporary_role(2, 20, &cancel)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    coordinator.release_temporary_role(1, SLOT_COUNT);
    coordinator.retain_temporary_role(2, 20, &cancel).unwrap();
    assert_eq!(coordinator.temporary_role_high_water().unwrap(), SLOT_COUNT);
}
