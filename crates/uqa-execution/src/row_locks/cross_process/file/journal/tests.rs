//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{
    FileLockCoordinator, LockStrength, PublishedRowChange, PublishedRowChangeKind, RowChangeTarget,
    CHANGE_ENTRY_SIZE,
};

const TABLE: u64 = 7;

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
    let coordinator = FileLockCoordinator::open(&directory.path().join("journal.db")).unwrap();
    coordinator
        .publish_changes(&[update(1), update(2)])
        .unwrap();
    assert_eq!(coordinator.change_sequence().unwrap(), 2);
    // The length a machine failure can leave behind while an entry is appended.
    coordinator
        .change_file
        .set_len(2 * CHANGE_ENTRY_SIZE + 17)
        .unwrap();
    let baseline = coordinator.change_sequence().unwrap();
    assert_eq!(baseline, 2);
    coordinator.publish_changes(&[update(3)]).unwrap();
    assert_eq!(
        coordinator.change_file.metadata().unwrap().len(),
        3 * CHANGE_ENTRY_SIZE
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
