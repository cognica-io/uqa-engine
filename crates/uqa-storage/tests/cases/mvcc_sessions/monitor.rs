//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A session keeps its committed snapshot while the commit monitor shows that nothing was committed.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use uqa_core::memory::BudgetedVec;

/// A snapshot that reports the monitor value it was captured at, and adopts a later value when it has a counter for that.
pub(super) struct MonitoredSnapshot {
    pub(super) source: MemoryRecordSnapshot,
    pub(super) monitor: AtomicU64,
    pub(super) adoptions: Option<Arc<AtomicUsize>>,
}

impl CommittedRecordSnapshot for MonitoredSnapshot {
    fn sequence(&self) -> CommitSequence {
        self.source.sequence()
    }
    fn reclamation_epoch(&self) -> Option<u64> {
        CommittedRecordSnapshot::reclamation_epoch(&self.source)
    }
    fn commit_monitor(&self) -> Option<u64> {
        Some(self.monitor.load(Ordering::Relaxed))
    }
    fn adopt_commit_monitor(&self, monitor: u64) -> bool {
        let Some(adoptions) = &self.adoptions else {
            return false;
        };
        self.monitor.store(monitor, Ordering::Relaxed);
        adoptions.fetch_add(1, Ordering::Relaxed);
        true
    }
    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        CommittedRecordSnapshot::get(&self.source, key, control)
    }
    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>> {
        CommittedRecordSnapshot::scan(&self.source, prefix, after, limit, control)
    }
}

fn monitored(value: u64) -> Arc<Persistence> {
    let persistence = Persistence::new();
    persistence.state.lock().monitor = Some(value);
    persistence
}

fn captures(persistence: &Persistence) -> usize {
    persistence.state.lock().captures
}

fn refresh(session: &VersionedKeyValueStore) {
    session
        .refresh_transaction_snapshot(session.retention_control().cancellation())
        .unwrap();
}

/// Commit through another session and move the monitor, as a provider's monitor moves with every commit.
fn commit_elsewhere(persistence: &Arc<Persistence>, key: &[u8], value: &[u8]) {
    persistence.session(1 << 20).put(key, value).unwrap();
    let mut state = persistence.state.lock();
    state.monitor = state.monitor.map(|monitor| monitor + 1);
}

#[test]
fn a_refresh_captures_no_snapshot_while_the_monitor_shows_no_commit() {
    let persistence = monitored(7);
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    session.put(b"own", b"private").unwrap();
    let begun = captures(&persistence);
    refresh(&session);
    refresh(&session);
    assert_eq!(captures(&persistence), begun);

    // A commit moves the monitor, and the next refresh captures the new boundary once.
    commit_elsewhere(&persistence, b"peer", b"first");
    let committed = captures(&persistence);
    assert!(session.get(b"peer").unwrap().is_none());
    refresh(&session);
    assert_eq!(captures(&persistence), committed + 1);
    assert_eq!(session.get(b"peer").unwrap().unwrap(), b"first");
    assert_eq!(session.get(b"own").unwrap().unwrap(), b"private");
    refresh(&session);
    assert_eq!(captures(&persistence), committed + 1);

    // A monitor that moves without a record commit costs one capture, which the session keeps for its newer value.
    persistence.state.lock().monitor = Some(100);
    refresh(&session);
    assert_eq!(captures(&persistence), committed + 2);
    refresh(&session);
    assert_eq!(captures(&persistence), committed + 2);
    assert_eq!(session.get(b"own").unwrap().unwrap(), b"private");
    session.commit_transaction().unwrap();
}

#[test]
fn a_snapshot_that_adopts_the_monitor_value_stays_in_place() {
    let persistence = monitored(7);
    let adoptions = Arc::new(AtomicUsize::new(0));
    persistence.state.lock().adoptions = Some(Arc::clone(&adoptions));
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    session.put(b"own", b"private").unwrap();
    let begun = captures(&persistence);
    // The monitor moves without a record commit. One capture finds the same sequence, and the session's snapshot takes the newer value instead of being replaced.
    persistence.state.lock().monitor = Some(100);
    refresh(&session);
    assert_eq!(captures(&persistence), begun + 1);
    assert_eq!(adoptions.load(Ordering::Relaxed), 1);
    refresh(&session);
    assert_eq!(captures(&persistence), begun + 1);
    assert_eq!(adoptions.load(Ordering::Relaxed), 1);
    // A record commit is a new boundary, which a snapshot of the earlier one cannot adopt.
    commit_elsewhere(&persistence, b"peer", b"committed");
    let committed = captures(&persistence);
    refresh(&session);
    assert_eq!(captures(&persistence), committed + 1);
    assert_eq!(adoptions.load(Ordering::Relaxed), 1);
    assert_eq!(session.get(b"peer").unwrap().unwrap(), b"committed");
    assert_eq!(session.get(b"own").unwrap().unwrap(), b"private");
    session.rollback_transaction().unwrap();
}

#[test]
fn a_snapshot_restored_by_a_savepoint_is_refreshed_from_its_own_monitor_value() {
    let persistence = monitored(1);
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    session.savepoint("early").unwrap();
    commit_elsewhere(&persistence, b"peer", b"committed");
    refresh(&session);
    assert_eq!(session.get(b"peer").unwrap().unwrap(), b"committed");
    // The savepoint holds the snapshot from before the commit, with the monitor value of that time.
    session.rollback_to_savepoint("early").unwrap();
    assert!(session.get(b"peer").unwrap().is_none());
    let restored = captures(&persistence);
    refresh(&session);
    assert_eq!(captures(&persistence), restored + 1);
    assert_eq!(session.get(b"peer").unwrap().unwrap(), b"committed");
    session.rollback_transaction().unwrap();
}

#[test]
fn a_persistence_without_a_monitor_captures_at_every_refresh() {
    let persistence = Persistence::new();
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    let begun = captures(&persistence);
    refresh(&session);
    refresh(&session);
    assert_eq!(captures(&persistence), begun + 2);
    session.rollback_transaction().unwrap();
}

#[test]
fn a_cancelled_refresh_fails_whether_or_not_it_would_capture() {
    let persistence = monitored(3);
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    assert!(session.refresh_transaction_snapshot(&cancellation).is_err());
    commit_elsewhere(&persistence, b"peer", b"committed");
    assert!(session.refresh_transaction_snapshot(&cancellation).is_err());
    assert!(session.get(b"peer").unwrap().is_none());
    session.rollback_transaction().unwrap();
}
