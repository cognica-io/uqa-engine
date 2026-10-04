//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::KeyValueReadRevision;
use crate::mvcc::{CommitSequence, DatabaseId};

fn committed(database: u8, sequence: u64) -> KeyValueReadRevision {
    KeyValueReadRevision::records(
        DatabaseId::from_bytes([database; 16]),
        CommitSequence::from_u64(sequence),
        None,
    )
}

#[test]
fn a_view_follows_only_the_immediately_preceding_commit_of_its_database() {
    assert!(committed(1, 8).follows_by_one_commit(&committed(1, 7)));
    assert!(!committed(1, 7).follows_by_one_commit(&committed(1, 7)));
    assert!(!committed(1, 9).follows_by_one_commit(&committed(1, 7)));
    assert!(!committed(1, 6).follows_by_one_commit(&committed(1, 7)));
    assert!(!committed(2, 8).follows_by_one_commit(&committed(1, 7)));
    // The session's own uncommitted changes may accompany the earlier view; the later view must be committed.
    let private = KeyValueReadRevision::records(
        DatabaseId::from_bytes([1; 16]),
        CommitSequence::from_u64(7),
        Some(crate::mvcc::PrivateRecordRevision::for_tests()),
    );
    assert!(committed(1, 8).follows_by_one_commit(&private));
    assert!(!KeyValueReadRevision::records(
        DatabaseId::from_bytes([1; 16]),
        CommitSequence::from_u64(8),
        Some(crate::mvcc::PrivateRecordRevision::for_tests()),
    )
    .follows_by_one_commit(&committed(1, 7)));
    assert!(private.same_committed_state(&committed(1, 7)));
    assert!(committed(1, 7).same_committed_state(&private));
    assert!(!private.same_committed_state(&committed(1, 8)));
    assert!(!committed(2, 7).same_committed_state(&committed(1, 7)));
    let unversioned = KeyValueReadRevision::fresh();
    assert!(!unversioned.follows_by_one_commit(&committed(1, 7)));
    assert!(!committed(1, 8).follows_by_one_commit(&unversioned));
    assert!(!unversioned.same_committed_state(&unversioned));
}
