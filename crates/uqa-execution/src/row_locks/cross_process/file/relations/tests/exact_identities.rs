//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full identity equality, live slot retention, bounded reuse and encryption.

use super::*;

#[test]
fn different_relations_with_colliding_hashes_never_conflict_between_processes() {
    let left = b"public.collision_3158";
    let right = b"public.collision_7316";
    let hash = crate::row_locks::cross_process::table_hash;
    assert_eq!(hash(left) % (1 << 20), hash(right) % (1 << 20));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("collisions.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start_for_relation(&path, right);
    for held in RelationLockMode::ALL {
        coordinator
            .try_relation_claim(PARENT_SESSION, left, held)
            .unwrap()
            .unwrap();
        for requested in RelationLockMode::ALL {
            assert_eq!(
                peer.request(&format!("try {}", requested as u8)),
                "granted",
                "{held:?}, {requested:?}"
            );
            assert_eq!(
                peer.request(&format!("release {}", requested as u8)),
                "released"
            );
        }
        coordinator.release_relation(PARENT_SESSION, left, held);
    }
}

#[test]
fn a_waiters_identity_stays_pinned_after_the_foreign_holder_exits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("waiting.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let cancel = uqa_core::CancellationToken::new();
    let mut holder = peer::Peer::start(&path);
    assert_eq!(holder.request("try 7"), "granted");
    let waiting = coordinator.pin_relation(RELATION, &cancel).unwrap();
    holder.terminate();
    let other = coordinator
        .pin_relation(b"different relation", &cancel)
        .unwrap();
    assert_ne!(waiting.slot(), other.slot());
    let released = waiting.slot();
    drop(waiting);
    let reused = coordinator
        .pin_relation(b"third relation", &cancel)
        .unwrap();
    assert_eq!(reused.slot(), released);
}

#[test]
fn a_row_only_observer_can_read_a_foreign_relation_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("observer.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(peer.request("try 7"), "granted");
    let claim = relation_wait_claim(0, RelationLockMode::AccessShare);
    assert_eq!(
        coordinator.current_relation_generation(claim.offset),
        Some(1)
    );
    assert!(matches!(
        coordinator
            .try_relation_claim(PARENT_SESSION, RELATION, RelationLockMode::AccessShare)
            .unwrap(),
        Err(RelationClaimWait::Conflict(_))
    ));
    assert_eq!(peer.request("release 7"), "released");
    coordinator
        .try_relation_claim(PARENT_SESSION, RELATION, RelationLockMode::AccessShare)
        .unwrap()
        .unwrap();
    coordinator.release_relation(PARENT_SESSION, RELATION, RelationLockMode::AccessShare);
}

#[test]
fn identity_comparison_keeps_binary_suffixes_and_registry_reuse_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("identities.db")).unwrap();
    let cancel = uqa_core::CancellationToken::new();
    let left = vec![0xff; 8192];
    let mut right = left.clone();
    right[8191] = 0;
    let first = coordinator.pin_relation(&left, &cancel).unwrap();
    let second = coordinator.pin_relation(&right, &cancel).unwrap();
    let prefix = coordinator.pin_relation(&right[..8191], &cancel).unwrap();
    assert_ne!(first.slot(), second.slot());
    assert_ne!(first.slot(), prefix.slot());
    assert_ne!(second.slot(), prefix.slot());
    drop((first, second, prefix));
    for number in 0..128 {
        let key = format!("short-lived relation {number}");
        let identity = coordinator.pin_relation(key.as_bytes(), &cancel).unwrap();
        assert_eq!(identity.slot(), 0);
    }
    assert_eq!(coordinator.relation_registry_counts(), (0, 3));
    assert!(std::fs::metadata(&coordinator.relation_path).unwrap().len() < 128 * 1024);
}

#[test]
fn encrypted_relation_identities_share_credentials_and_reject_live_mismatches() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("encrypted.db");
    let key = "relation-registry-test-key";
    let relation = b"private_relation_marker_that_must_not_be_plaintext";
    let coordinator = FileLockCoordinator::open_with_key(
        &path,
        Some(uqa_storage::StorageEncryptionKey::new(key)),
    )
    .unwrap();
    coordinator
        .try_relation_claim(PARENT_SESSION, relation, RelationLockMode::AccessExclusive)
        .unwrap()
        .unwrap();
    let mut peer = peer::Peer::start_encrypted(&path, relation, key);
    assert!(peer.request("try 0").starts_with("conflict "));
    let mut wrong = peer::Peer::start_encrypted(&path, relation, "another-test-key");
    assert!(wrong.request("try 0").starts_with("error "));
    coordinator.release_relation(PARENT_SESSION, relation, RelationLockMode::AccessExclusive);
    assert_eq!(peer.request("try 0"), "granted");
    assert_eq!(peer.request("release 0"), "released");
    drop((peer, wrong));
    let registry = coordinator.relation_path.clone();
    drop(coordinator);
    let bytes = std::fs::read(registry).unwrap();
    assert!(!bytes
        .windows(relation.len())
        .any(|window| window == relation));
}
