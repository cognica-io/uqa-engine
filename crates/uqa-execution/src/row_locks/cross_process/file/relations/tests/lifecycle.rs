//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic relation admission and SQL lock-manager wait lifecycle.

use super::*;

#[test]
fn conditional_manager_acquisition_checks_foreign_holders_and_preserves_prior_claims() {
    use crate::row_locks::RowLockManager;
    use RelationLockMode::{RowExclusive, Share};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let manager = RowLockManager::for_database_file(&path);
    let relation = manager.table_key(std::str::from_utf8(RELATION).unwrap());
    let cancel = uqa_core::CancellationToken::new();
    let mut peer = peer::Peer::start(&path);
    for (held, expected) in RelationLockMode::ALL.into_iter().zip(CONFLICTS) {
        assert_eq!(peer.request(&format!("try {}", held as u8)), "granted");
        for (wanted, conflict) in RelationLockMode::ALL.into_iter().zip(expected.bytes()) {
            assert_eq!(
                manager
                    .try_acquire_relation(PARENT_SESSION, relation, wanted, 0, &cancel)
                    .unwrap(),
                conflict != b'X',
                "{held:?}, {wanted:?}"
            );
            assert!(!manager.waiting_for_relation(PARENT_SESSION, relation));
            assert!(manager.state.lock().advertised_waits.is_empty());
            manager.release_session(PARENT_SESSION);
        }
        assert_eq!(peer.request(&format!("release {}", held as u8)), "released");
    }
    manager
        .acquire_relation(PARENT_SESSION, relation, RowExclusive, 0, &cancel)
        .unwrap();
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    assert_eq!(peer.request(&format!("wait {}", Share as u8)), "waiting");
    assert!(!manager
        .try_acquire_relation(PARENT_SESSION, relation, Share, 1, &cancel)
        .unwrap());
    assert_eq!(peer.request("clear"), "cleared");
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
    assert_eq!(
        peer.request(&format!("try {}", Share as u8)),
        format!(
            "conflict {}",
            relation_mode_claim(RELATION, RowExclusive, true).offset
        )
    );
    assert!(manager
        .try_acquire_relation(PARENT_SESSION, relation, Share, 1, &cancel)
        .unwrap());
    manager.release_mark_above(PARENT_SESSION, 0);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
    manager.release_session(PARENT_SESSION);
}

#[test]
fn admission_closes_the_gap_between_conflict_checks_and_holder_publication() {
    use RelationLockMode::{RowExclusive, Share};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = peer::Peer::start(&path);
    coordinator
        .apply_byte_mode(RELATION_ADMISSION_BYTE, None, Some(true))
        .unwrap();
    let mut admission = Admission {
        coordinator: &coordinator,
        active: true,
    };
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "admission busy"
    );
    coordinator
        .try_admitted_relation(
            &mut coordinator.state.lock(),
            PARENT_SESSION,
            RELATION,
            Share,
        )
        .unwrap()
        .unwrap();
    admission.release().unwrap();
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        format!(
            "conflict {}",
            relation_mode_claim(RELATION, Share, true).offset
        )
    );
    release(&coordinator, Share);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
}

#[test]
fn waiting_relation_upgrades_cancel_without_false_deadlocks_and_report_real_cycles() {
    use crate::row_locks::RowLockManager;
    use RelationLockMode::{RowExclusive, Share};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relations.db");
    let manager = std::sync::Arc::new(RowLockManager::for_database_file(&path));
    let relation = manager.table_key(std::str::from_utf8(RELATION).unwrap());
    let cancel = std::sync::Arc::new(uqa_core::CancellationToken::new());
    manager
        .acquire_relation(PARENT_SESSION, relation, RowExclusive, 0, &cancel)
        .unwrap();
    let mut peer = peer::Peer::start(&path);
    assert_eq!(
        peer.request(&format!("try {}", RowExclusive as u8)),
        "granted"
    );
    let writer = manager.clone();
    let worker_cancel = cancel.clone();
    let (send, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(writer.acquire_relation(PARENT_SESSION, relation, Share, 1, &worker_cancel))
            .unwrap();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut early = None;
    while !manager.waiting_for_relation(PARENT_SESSION, relation)
        && std::time::Instant::now() < deadline
    {
        if let Ok(result) = done.try_recv() {
            early = Some(result);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let was_waiting = manager.waiting_for_relation(PARENT_SESSION, relation);
    cancel.cancel();
    worker.join().unwrap();
    assert!(
        early.is_none(),
        "an idle foreign holder must not form a cycle: {early:?}"
    );
    assert!(was_waiting);
    assert_eq!(done.recv().unwrap().unwrap_err().sqlstate(), Some("57014"));
    assert!(!manager.waiting_for_relation(PARENT_SESSION, relation));
    assert!(manager.state.lock().advertised_waits.is_empty());

    assert_eq!(peer.request(&format!("wait {}", Share as u8)), "waiting");
    let error = manager
        .acquire_relation(
            PARENT_SESSION,
            relation,
            Share,
            1,
            &uqa_core::CancellationToken::new(),
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("40P01"));
    assert_eq!(peer.request("clear"), "cleared");
    assert_eq!(
        peer.request(&format!("try {}", Share as u8)),
        format!(
            "conflict {}",
            relation_mode_claim(RELATION, RowExclusive, true).offset
        )
    );
    manager.release_session(PARENT_SESSION);
    assert_eq!(
        peer.request(&format!("release {}", RowExclusive as u8)),
        "released"
    );
}
