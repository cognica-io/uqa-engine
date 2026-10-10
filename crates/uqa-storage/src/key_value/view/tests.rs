//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::KeyValueReadRevision;
use crate::mvcc::{CommitSequence, DatabaseId};

#[test]
fn value_batch_default_uses_required_point_reads_and_stops_before_the_next_key() {
    use super::*;
    use std::cell::Cell;

    struct Legacy {
        control: StorageReadControl,
        calls: Cell<usize>,
    }
    impl KeyValueRead for Legacy {
        fn control(&self) -> &StorageReadControl {
            &self.control
        }
        fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
            Ok(KeyValueReadRevision::fresh())
        }
        fn visit_value(
            &self,
            key: &[u8],
            visit: &mut ValueReadVisitor<'_>,
        ) -> StorageBackendResult<()> {
            self.calls.set(self.calls.get() + 1);
            visit((key == b"found").then_some(b"value".as_slice()))
        }
        fn visit_prefix(
            &self,
            _: &[u8],
            _: &mut KeyValueReadVisitor<'_>,
        ) -> StorageBackendResult<()> {
            panic!("point batches do not require prefix reads")
        }
    }
    let provider = Legacy {
        control: StorageReadControl::with_limit(1024),
        calls: Cell::new(0),
    };
    let mut requested = [b"missing".as_slice(), b"found", b"found"]
        .into_iter()
        .map(|key| {
            let mut bytes = BudgetedVec::new(provider.control.memory());
            bytes.extend_from_slice(key)?;
            Ok(bytes)
        })
        .chain(std::iter::from_fn(|| {
            panic!("a stopped visitor cannot generate the next key")
        }));
    let mut calls = 0;
    provider
        .visit_values(&mut requested, &mut |key, value| {
            assert_eq!(value, (key == b"found").then_some(b"value".as_slice()));
            calls += 1;
            Ok(calls < 3)
        })
        .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(provider.calls.get(), 3);
    assert_eq!(provider.control.memory().used(), 0);
}

fn committed(database: u8, sequence: u64) -> KeyValueReadRevision {
    KeyValueReadRevision::records(
        DatabaseId::from_bytes([database; 16]),
        CommitSequence::from_u64(sequence),
        None,
    )
}

#[test]
fn commit_visibility_requires_the_same_database_and_a_sufficient_snapshot() {
    let database = DatabaseId::from_bytes([1; 16]);
    let sequence = CommitSequence::from_u64(7);
    assert!(committed(1, 7).includes_commit(database, sequence));
    assert!(committed(1, 8).includes_commit(database, sequence));
    assert!(!committed(1, 6).includes_commit(database, sequence));
    assert!(!committed(2, 8).includes_commit(database, sequence));
    assert!(!KeyValueReadRevision::fresh().includes_commit(database, sequence));
    let private = KeyValueReadRevision::records(
        database,
        sequence,
        Some(crate::mvcc::PrivateRecordRevision::for_tests()),
    );
    assert!(private.includes_commit(database, sequence));
    assert!(!private.includes_commit(database, CommitSequence::from_u64(8)));
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
