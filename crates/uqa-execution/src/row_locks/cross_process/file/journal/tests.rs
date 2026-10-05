//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use super::super::{
    FileLockCoordinator, LockStrength, PublishedRowChange, PublishedRowChangeKind, RowChangeTarget,
    CHANGE_ENTRY_SIZE,
};
use super::log::HEADER_BYTES;

const TABLE: u64 = 7;

mod peer;

fn update(doc_id: u64) -> PublishedRowChange {
    PublishedRowChange {
        table_hash: TABLE,
        doc_id,
        kind: PublishedRowChangeKind::Update,
        strength: LockStrength::ForUpdate,
    }
}

#[test]
fn a_journal_ending_inside_an_entry_counts_whole_entries_and_is_written_over() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator =
        Arc::new(FileLockCoordinator::open(&directory.path().join("journal.db")).unwrap());
    coordinator
        .publish_changes(&[update(1), update(2)])
        .unwrap();
    assert_eq!(coordinator.change_sequence().unwrap(), 2);
    // The length a machine failure can leave behind while an entry is appended.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&coordinator.change_path)
        .unwrap()
        .set_len(2 * CHANGE_ENTRY_SIZE + 17)
        .unwrap();
    let (baseline, _lease) = coordinator.pin_change_sequence().unwrap();
    assert_eq!(baseline, 2);
    coordinator.publish_changes(&[update(3)]).unwrap();
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        HEADER_BYTES + CHANGE_ENTRY_SIZE
    );
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 3, baseline, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Present(3)
    );
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Unchanged
    );
}

#[test]
fn completed_readers_reclaim_history_and_sequences_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.db");
    let coordinator = Arc::new(FileLockCoordinator::open(&path).unwrap());
    for round in 0..100 {
        let (sequence, lease) = coordinator.pin_change_sequence().unwrap();
        assert_eq!(sequence, round);
        coordinator.publish_changes(&[update(round)]).unwrap();
        assert_eq!(
            coordinator
                .change_target_after(TABLE, round, sequence, LockStrength::ForUpdate)
                .unwrap(),
            RowChangeTarget::Present(round)
        );
        let retained = Arc::clone(&lease);
        drop(lease);
        assert!(std::fs::metadata(&coordinator.change_path).unwrap().len() >= CHANGE_ENTRY_SIZE);
        drop(retained);
        assert_eq!(
            std::fs::metadata(&coordinator.change_path).unwrap().len(),
            HEADER_BYTES
        );
    }
    drop(coordinator);
    let reopened = Arc::new(FileLockCoordinator::open(&path).unwrap());
    assert_eq!(reopened.pin_change_sequence().unwrap().0, 100);
}

#[test]
fn advancing_live_readers_reclaims_prefixes_without_an_idle_interval() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator =
        Arc::new(FileLockCoordinator::open(&directory.path().join("overlap.db")).unwrap());
    let (_, oldest) = coordinator.pin_change_sequence().unwrap();
    coordinator
        .publish_changes(&(0..2048).map(update).collect::<Vec<_>>())
        .unwrap();
    let (sequence, later) = coordinator.pin_change_sequence().unwrap();
    coordinator.publish_changes(&[update(9000)]).unwrap();
    drop(oldest);
    let log = super::log::Log::open(&coordinator.change_path).unwrap();
    assert_eq!(log.base, sequence);
    assert_eq!(log.next, 2049);
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        HEADER_BYTES + CHANGE_ENTRY_SIZE
    );
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 9000, sequence, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Present(9000)
    );
    drop(later);
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        HEADER_BYTES
    );
}

#[test]
fn reclamation_preserves_move_delete_and_reused_key_histories() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator =
        Arc::new(FileLockCoordinator::open(&directory.path().join("moves.db")).unwrap());
    let (baseline, lease) = coordinator.pin_change_sequence().unwrap();
    let moved = PublishedRowChange {
        kind: PublishedRowChangeKind::Rewrite(super::super::PublishedRowIdentity {
            table_hash: TABLE,
            doc_id: 2,
        }),
        ..update(1)
    };
    coordinator.publish_changes(&[moved, update(2)]).unwrap();
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Present(2)
    );
    coordinator
        .publish_changes(&[
            PublishedRowChange {
                kind: PublishedRowChangeKind::Delete,
                ..update(2)
            },
            update(1),
        ])
        .unwrap();
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Deleted
    );
    drop(lease);
    assert!(coordinator
        .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
        .unwrap_err()
        .contains("reader lease"));
    let (fresh, _lease) = coordinator.pin_change_sequence().unwrap();
    coordinator.publish_changes(&[update(1)]).unwrap();
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 1, fresh, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Present(1)
    );
}

#[test]
fn incomplete_reclamation_scratch_does_not_replace_the_live_generation() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator =
        Arc::new(FileLockCoordinator::open(&directory.path().join("scratch.db")).unwrap());
    let (baseline, lease) = coordinator.pin_change_sequence().unwrap();
    coordinator.publish_changes(&[update(1)]).unwrap();
    let mut scratch = coordinator.change_path.as_os_str().to_owned();
    scratch.push(".reclaim");
    std::fs::write(&scratch, b"UQACHG02partial").unwrap();
    assert_eq!(
        coordinator
            .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
            .unwrap(),
        RowChangeTarget::Present(1)
    );
    drop(lease);
    assert!(!std::path::Path::new(&scratch).exists());
    assert_eq!(coordinator.change_sequence().unwrap(), 1);
}
