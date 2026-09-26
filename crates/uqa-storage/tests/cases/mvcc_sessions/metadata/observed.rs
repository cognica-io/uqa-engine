//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::{KeyValueRead, KeyValueReadRevision};

fn revision(store: &dyn KeyValueStore, key: &[u8]) -> KeyValueReadRevision {
    let mut result = None;
    store
        .with_read_view(&mut |read| {
            result = read.record_revision(key)?;
            Ok(())
        })
        .unwrap();
    result.unwrap()
}

#[test]
fn observed_metadata_preserves_the_data_snapshot_and_savepoint_undo() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let peer = persistence.session(1 << 20);
    peer.put(b"row", b"old").unwrap();
    store.begin_transaction().unwrap();
    assert_eq!(store.get(b"row").unwrap().as_deref(), Some(&b"old"[..]));
    peer.put(b"row", b"new").unwrap();
    peer.put(b"sealed", b"ready").unwrap();
    let original = revision(&peer, b"sealed");
    store.savepoint("metadata").unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.require_observed(b"sealed", &original)?;
            batch.put_observed(b"sealed", b"published", &original)?;
            batch.require_unchanged(b"sealed")?;
            batch.put(b"head", b"selected")
        })
        .unwrap();
    assert_eq!(
        store.get(b"sealed").unwrap().as_deref(),
        Some(&b"published"[..])
    );
    assert_eq!(store.get(b"row").unwrap().as_deref(), Some(&b"old"[..]));
    store.rollback_to_savepoint("metadata").unwrap();
    assert_eq!(store.get(b"sealed").unwrap(), None);
    assert_eq!(store.get(b"head").unwrap(), None);
    store
        .with_mutation(&mut |_, batch| {
            batch.put_observed(b"sealed", b"published", &original)?;
            batch.put(b"head", b"selected")
        })
        .unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(
        peer.get(b"sealed").unwrap().as_deref(),
        Some(&b"published"[..])
    );
    assert_eq!(peer.get(b"row").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn observed_metadata_rejects_stale_private_foreign_and_conflicting_provenance() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let peer = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    peer.put(b"sealed", b"one").unwrap();
    let original = revision(&peer, b"sealed");
    for observed_first in [true, false] {
        assert!(store
            .with_mutation(&mut |_, batch| {
                if observed_first {
                    batch.require_observed(b"sealed", &original)?;
                    batch.require_unchanged(b"sealed")?;
                } else {
                    batch.require_unchanged(b"sealed")?;
                    batch.require_observed(b"sealed", &original)?;
                }
                batch.put(b"partial", b"forbidden")
            })
            .is_err());
        assert_eq!(store.get(b"partial").unwrap(), None);
    }
    store
        .with_mutation(&mut |_, batch| batch.put_observed(b"sealed", b"two", &original))
        .unwrap();
    peer.put(b"sealed", b"competitor").unwrap();
    assert!(store.commit_transaction().is_err());
    store.rollback_transaction().unwrap();
    assert_eq!(
        peer.get(b"sealed").unwrap().as_deref(),
        Some(&b"competitor"[..])
    );
    peer.begin_transaction().unwrap();
    peer.put(b"sealed", b"private").unwrap();
    let private = revision(&peer, b"sealed");
    let foreign = Persistence::new().session(1 << 20);
    foreign.put(b"sealed", b"foreign").unwrap();
    let foreign_view = foreign.record_snapshot().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    // The persistence fixture otherwise assigns the same history to every instance.
    let foreign_read = RecordRead::new(&foreign_view, DatabaseId::from_bytes([10; 16]), &control);
    let foreign_revision = foreign_read.record_revision(b"sealed").unwrap().unwrap();
    for invalid in [private, foreign_revision, KeyValueReadRevision::fresh()] {
        assert!(store
            .with_mutation(&mut |_, batch| batch.put_observed(b"sealed", b"bad", &invalid))
            .is_err());
    }
    peer.rollback_transaction().unwrap();
    store.begin_transaction().unwrap();
    store.put(b"sealed", b"private").unwrap();
    let committed = revision(&peer, b"sealed");
    assert!(store
        .with_mutation(&mut |_, batch| batch.put_observed(b"sealed", b"bad", &committed))
        .is_err());
    assert_eq!(
        store.get(b"sealed").unwrap().as_deref(),
        Some(&b"private"[..])
    );
    store.rollback_transaction().unwrap();
}

#[test]
fn observed_metadata_resolves_the_original_commit_after_lost_replies() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store = persistence.session(1 << 20);
        let peer = persistence.session(1 << 20);
        store.begin_transaction().unwrap();
        peer.put(b"sealed", b"ready").unwrap();
        let original = revision(&peer, b"sealed");
        store
            .with_mutation(&mut |_, batch| {
                batch.put_observed(b"sealed", b"published", &original)?;
                batch.put(b"head", b"selected")
            })
            .unwrap();
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(store.commit_transaction().is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        store.commit_transaction().unwrap();
        let state = persistence.state.lock();
        assert_eq!(state.attempts[before], *state.attempts.last().unwrap());
        drop(state);
        assert_eq!(
            peer.get(b"sealed").unwrap().as_deref(),
            Some(&b"published"[..])
        );
        assert_eq!(
            peer.get(b"head").unwrap().as_deref(),
            Some(&b"selected"[..])
        );
    }
}
