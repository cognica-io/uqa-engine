//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A killed owner releases liveness but never turns a durable commit into an abort.

use uqa_storage::mvcc::StorageTransactionId;

use super::super::liveness::{admit, prepare};
use super::*;

fn child(path: &Path, mode: usize) {
    let connection = open(path, mode);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let active = admit(&store, &control);
    let pending = admit(&store, &control);
    let committed = admit(&store, &control);
    let (pending_id, pending_write) = prepare(&store, &pending, b"pending", &control);
    let (committed_id, committed_write) = prepare(&store, &committed, b"committed", &control);
    event("participants-retained");
    event(&format!(
        "identities {} {} {} {} {}",
        active.id().allocation(),
        pending.id().allocation(),
        committed.id().allocation(),
        pending_id.allocation(),
        committed_id.allocation(),
    ));
    for command in std::io::stdin().lock().lines() {
        match command.unwrap().as_str() {
            "commit" => {
                let held = store.serializable_admission(&control).unwrap();
                store
                    .commit(committed_id, &committed_write, &control)
                    .unwrap();
                drop(held);
                event("records-committed");
            }
            other => panic!("unexpected participant peer command {other}"),
        }
    }
    // A killed process cannot run these destructors; the OS must release its byte leases.
    drop((active, pending, committed, pending_write, committed_write));
}

#[test]
fn process_death_recovers_only_dead_owners_and_preserves_committed_data() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        child(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
        );
        return;
    }
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("liveness.db");
        let connection = open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        let survivor = admit(&store, &control);
        let mut peer = Peer::start_test(
            &path,
            mode,
            concat!(
                module_path!(),
                "::process_death_recovers_only_dead_owners_and_preserves_committed_data"
            ),
            "participants-retained",
        );
        let numbers: Vec<u64> = peer
            .expect_prefix("identities ")
            .split_whitespace()
            .map(|number| number.parse().unwrap())
            .collect();
        let [active, pending, committed, pending_physical, committed_physical] = numbers[..] else {
            panic!("incomplete participant identities");
        };
        let actor = |allocation| {
            SerializableTransactionId::new(store.identity, survivor.id().coordinator(), allocation)
                .unwrap()
        };
        let pending_id = StorageTransactionId::new(store.identity, pending_physical).unwrap();
        let committed_id = StorageTransactionId::new(store.identity, committed_physical).unwrap();
        store.recover_serializable(&control).unwrap();
        let held = store.serializable_admission(&control).unwrap();
        held.graph().check_active(actor(active)).unwrap();
        assert!(matches!(
            held.graph().check_active(actor(pending)),
            Err(VersionError::TransactionSealed)
        ));
        let publication = held.graph().publication(actor(committed)).unwrap().unwrap();
        assert_eq!(
            store.commit_status(pending_id, &control).unwrap(),
            CommitStatus::Pending
        );
        drop(held);
        peer.command("commit", "records-committed");
        peer.child.kill().unwrap();
        assert!(!peer.child.wait().unwrap().success());
        let (later, snapshot) = store
            .admit_serializable(true, &control, || {
                assert_eq!(
                    store.commit_status(pending_id, &control).unwrap(),
                    CommitStatus::Aborted
                );
                store.snapshot(&control)
            })
            .unwrap();
        assert!(later.id().allocation() > committed);
        let outcome = store.commit_status(committed_id, &control).unwrap();
        assert!(matches!(outcome, CommitStatus::Committed(_)));
        assert!(snapshot.get(b"pending", &control).unwrap().is_none());
        let record = snapshot.get(b"committed", &control).unwrap().unwrap();
        assert_eq!(record.value().map(|value| &***value), Some(&b"durable"[..]));
        drop(record);
        let mut held = store.serializable_admission(&control).unwrap();
        held.graph().check_active(survivor.id()).unwrap();
        assert!(held.graph().check_active(actor(active)).is_err());
        assert_eq!(
            held.graph_mut()
                .resolve_publication(publication, CommitStatus::Unknown)
                .unwrap(),
            outcome
        );
        drop((held, survivor, later, snapshot));
        store.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}
