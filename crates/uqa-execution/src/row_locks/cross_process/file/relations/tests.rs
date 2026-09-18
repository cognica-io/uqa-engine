//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-lock admission and holder accounting across independent processes.

use super::*;
use crate::row_locks::cross_process::relation_wait_claim;

mod lifecycle;
mod peer;

const RELATION: &[u8] = b"public.relation_lock_test";
const PARENT_SESSION: u64 = 101;
const PEER_SESSION: u64 = 202;
const CONFLICTS: [&str; 8] = [
    ".......X", "......XX", "....XXXX", "...XXXXX", "..XX.XXX", "..XXXXXX", ".XXXXXXX", "XXXXXXXX",
];

fn held(coordinator: &FileLockCoordinator, mode: RelationLockMode) {
    coordinator
        .try_relation_claim(PARENT_SESSION, RELATION, mode)
        .unwrap()
        .unwrap();
}

fn release(coordinator: &FileLockCoordinator, mode: RelationLockMode) {
    coordinator.release(PARENT_SESSION, &relation_byte_claims(RELATION, mode));
}

#[test]
fn relation_holder_accounting_matches_all_eight_modes_within_one_process() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("relations.db")).unwrap();
    for (left, expected) in RelationLockMode::ALL.into_iter().zip(CONFLICTS) {
        held(&coordinator, left);
        for (right, expected) in RelationLockMode::ALL.into_iter().zip(expected.bytes()) {
            let result = coordinator
                .try_relation_claim(PEER_SESSION, RELATION, right)
                .unwrap();
            if expected == b'X' {
                assert_eq!(
                    result,
                    Err(RelationClaimWait::Conflict(relation_mode_claim(
                        RELATION, left, true
                    ))),
                    "{left:?}, {right:?}"
                );
            } else {
                result.unwrap();
                coordinator.release(PEER_SESSION, &relation_byte_claims(RELATION, right));
            }
        }
        release(&coordinator, left);
    }
    let state = coordinator.state.lock();
    assert!(state.claims.is_empty());
    assert!(state.holders.is_empty());
    assert!(state.holder_slots.is_empty());
}

#[test]
fn all_eight_relation_modes_match_postgresql_across_processes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    for (left, expected) in RelationLockMode::ALL.into_iter().zip(CONFLICTS) {
        held(&coordinator, left);
        for (right, expected) in RelationLockMode::ALL.into_iter().zip(expected.bytes()) {
            let result = peer.request(&format!("try {}", right as u8));
            if expected == b'X' {
                assert_eq!(
                    result,
                    format!(
                        "conflict {}",
                        relation_mode_claim(RELATION, left, true).offset
                    ),
                    "{left:?}, {right:?}"
                );
            } else {
                assert_eq!(result, "granted", "{left:?}, {right:?}");
                assert_eq!(
                    peer.request(&format!("release {}", right as u8)),
                    "released"
                );
            }
        }
        release(&coordinator, left);
    }
}

#[test]
fn mixed_self_acquisitions_and_failed_upgrades_preserve_foreign_conflicts() {
    use RelationLockMode::{RowExclusive, Share};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    held(&coordinator, RowExclusive);
    held(&coordinator, Share);
    for (wanted, blocker) in [(Share, RowExclusive), (RowExclusive, Share)] {
        assert_eq!(
            peer.request(&format!("try {}", wanted as u8)),
            format!(
                "conflict {}",
                relation_mode_claim(RELATION, blocker, true).offset
            )
        );
    }
    release(&coordinator, Share);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    let blocked = Err(RelationClaimWait::Conflict(relation_mode_claim(
        RELATION,
        RowExclusive,
        true,
    )));
    assert_eq!(
        coordinator
            .try_relation_claim(PARENT_SESSION, RELATION, Share)
            .unwrap(),
        blocked
    );
    // The failed upgrade must leave our original ROW EXCLUSIVE holder visible to the peer's own upgrade.
    assert_eq!(
        peer.request(&format!("try {}", Share as u8)),
        format!(
            "conflict {}",
            relation_mode_claim(RELATION, RowExclusive, true).offset
        )
    );
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
    held(&coordinator, Share);
    release(&coordinator, Share);
    release(&coordinator, RowExclusive);
}

#[test]
fn relation_upgrade_deadlock_follows_the_actual_conflicting_mode_across_processes() {
    use RelationLockMode::{RowExclusive, Share};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    held(&coordinator, RowExclusive);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    let Err(RelationClaimWait::Conflict(_)) = coordinator
        .try_relation_claim(PARENT_SESSION, RELATION, Share)
        .unwrap()
    else {
        panic!("competing ROW EXCLUSIVE must block SHARE");
    };
    let wanted = relation_wait_claim(RELATION, Share);
    assert!(!coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(peer.request(&format!("wait {}", Share as u8)), "waiting");
    assert!(coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(peer.request("clear"), "cleared");
    assert!(!coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
    release(&coordinator, RowExclusive);
}

#[test]
fn a_second_conflicting_mode_can_close_a_foreign_cycle_while_the_first_holder_is_idle() {
    use RelationLockMode::{Exclusive, RowExclusive, RowShare, Share, ShareUpdateExclusive};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    held(&coordinator, RowShare);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    assert_eq!(
        peer.request(&format!("try {} 203", ShareUpdateExclusive as u8)),
        "granted"
    );
    let wanted = relation_wait_claim(RELATION, Share);
    assert!(!coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(
        peer.request(&format!("wait {} 203", Exclusive as u8)),
        "waiting"
    );
    assert!(coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(peer.request("clear 0 203"), "cleared");
    assert!(!coordinator.wait_cycle_reaches_session(PARENT_SESSION, wanted, &|_| None));
    assert_eq!(
        peer.request(&format!("release {} 203", ShareUpdateExclusive as u8)),
        "released"
    );
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
    release(&coordinator, RowShare);
}

#[test]
fn crashed_process_releases_relation_admission_and_its_held_modes() {
    use RelationLockMode::{AccessExclusive, ShareUpdateExclusive};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(
        peer.request(&format!("try {}", ShareUpdateExclusive as u8)),
        "granted"
    );
    assert_eq!(peer.request("admission"), "admitted");
    assert_eq!(
        coordinator
            .try_relation_claim(PARENT_SESSION, RELATION, AccessExclusive)
            .unwrap(),
        Err(RelationClaimWait::AdmissionBusy)
    );
    peer.terminate();
    held(&coordinator, AccessExclusive);
    release(&coordinator, AccessExclusive);
}

#[test]
fn relation_mode_offsets_do_not_overlap_row_pairs_or_exceed_supported_offsets() {
    use crate::row_locks::cross_process::{row_span_for_offset_width, RELATION_SPAN, ROW_BASE};
    for (width, maximum) in [(4, i32::MAX as u64), (8, i64::MAX as u64)] {
        let row_end = ROW_BASE + 2 * row_span_for_offset_width(width);
        let last_mode = row_end + 8 * RELATION_SPAN - 1;
        assert!(last_mode > row_end);
        assert!(last_mode <= maximum);
    }
    let claims = RelationLockMode::ALL.map(|mode| relation_mode_claim(RELATION, mode, false));
    for pair in claims.windows(2) {
        assert_eq!(pair[1].offset, pair[0].offset + 1);
    }
}

#[test]
fn shared_objects_keep_typed_addresses_across_processes() {
    use crate::row_locks::{shared_objects::SharedCatalogLock, RowLockManager};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared-objects.db");
    let manager = RowLockManager::for_database_file(&path);
    let key = manager.shared_catalog_key(SharedCatalogLock::Object {
        class_id: 1260,
        oid: 20_001,
    });
    let mut peer = peer::Peer::start_for_relation(&path, &manager.relation_bytes(key));
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_relation(1, key, RelationLockMode::AccessShare, 1, &cancel)
        .unwrap();
    assert!(peer.request("try 7").starts_with("conflict "));
    manager.release_mark_above(1, 0);
    assert_eq!(peer.request("try 7"), "granted");
    assert!(!manager
        .try_acquire_relation(1, key, RelationLockMode::AccessShare, 0, &cancel)
        .unwrap());
    assert_eq!(peer.request("release 7"), "released");
    assert!(manager
        .try_acquire_relation(1, key, RelationLockMode::AccessShare, 0, &cancel)
        .unwrap());
}
