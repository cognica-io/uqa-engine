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

/// Commit through another session, which moves the monitor as every commit does.
fn commit_elsewhere(persistence: &Arc<Persistence>, key: &[u8], value: &[u8]) {
    persistence.session(1 << 20).put(key, value).unwrap();
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
fn the_latest_sequence_is_read_from_the_monitor_while_nothing_was_committed() {
    let persistence = monitored(7);
    let session = persistence.session(1 << 20);
    let first = session.change_version().unwrap();
    let captured = captures(&persistence);
    assert_eq!(session.change_version().unwrap(), first);
    assert_eq!(captures(&persistence), captured);

    // A commit moves the monitor, and the next answer takes one capture.
    commit_elsewhere(&persistence, b"peer", b"committed");
    let committed = captures(&persistence);
    let second = session.change_version().unwrap();
    assert!(second > first);
    assert_eq!(captures(&persistence), committed + 1);
    assert_eq!(session.change_version().unwrap(), second);
    assert_eq!(captures(&persistence), committed + 1);

    // A transaction answers with its own view, whatever was committed since it began, and its capture serves the session afterwards.
    commit_elsewhere(&persistence, b"peer", b"again");
    session.begin_transaction().unwrap();
    let begun = captures(&persistence);
    let third = session.change_version().unwrap();
    assert!(third > second);
    commit_elsewhere(&persistence, b"peer", b"later");
    assert_eq!(session.change_version().unwrap(), third);
    session.rollback_transaction().unwrap();
    let rolled_back = captures(&persistence);
    assert!(session.change_version().unwrap() > third);
    assert_eq!(captures(&persistence), rolled_back + 1);
    assert!(rolled_back > begun);

    // The session's own commit moves the monitor as well.
    let before = session.change_version().unwrap();
    session.put(b"own", b"committed").unwrap();
    assert!(session.change_version().unwrap() > before);

    // Without a monitor every answer captures.
    let unmonitored = Persistence::new();
    let session = unmonitored.session(1 << 20);
    session.change_version().unwrap();
    let captured = captures(&unmonitored);
    session.change_version().unwrap();
    assert_eq!(captures(&unmonitored), captured + 1);
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
