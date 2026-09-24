//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A killed owner releases liveness but never turns a durable commit into an abort.

use uqa_storage::mvcc::StorageTransactionId;

use super::super::liveness::{admit, prepare_records};
use super::*;

fn record_store(path: &Path, mode: usize, initialize: bool) -> SQLiteRecordStore {
    let connection = open(path, mode % 4);
    if mode < 4 {
        SQLiteRecordStore::new(&connection).unwrap()
    } else {
        if initialize {
            crate::Catalog::open(connection.clone()).unwrap();
        }
        SQLiteRecordStore::for_native(&connection, &control()).unwrap()
    }
}

fn record(
    store: &SQLiteRecordStore,
    key: &[u8],
    control: &StorageReadControl,
) -> (Vec<u8>, Vec<u8>) {
    use crate::mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner};
    use rusqlite::types::ValueRef;
    let Some(namespace) = store.native_namespace() else {
        return (key.to_vec(), b"durable".to_vec());
    };
    let record = NativeRecord::encode(
        NativeRecordFamily::Metadata,
        NativeRecordOwner::Database(namespace),
        &[ValueRef::Text(key), ValueRef::Text(b"durable")],
        control,
    )
    .unwrap();
    (record.key().to_vec(), record.row().to_vec())
}

fn child(path: &Path, mode: usize) {
    let store = record_store(path, mode, false);
    let control = control();
    let active = admit(&store, &control);
    let pending = admit(&store, &control);
    let committed = admit(&store, &control);
    let (pending_key, pending_value) = record(&store, b"pending", &control);
    let (committed_key, committed_value) = record(&store, b"committed", &control);
    let (pending_id, pending_write) = prepare_records(
        &store,
        &pending,
        &[RecordWrite {
            key: &pending_key,
            expected: None,
            value: Some(&pending_value),
        }],
        &control,
    );
    let (committed_id, committed_write) = prepare_records(
        &store,
        &committed,
        &[RecordWrite {
            key: &committed_key,
            expected: None,
            value: Some(&committed_value),
        }],
        &control,
    );
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
            "stage-main" => {
                let _held = store.serializable_admission(&control).unwrap();
                store.connection.with_physical::<()>(|connection| {
                    connection.create_scalar_function(
                        "__uqa_test_main_commit_gate",
                        0,
                        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                        |_| -> rusqlite::Result<i64> {
                            event("main-records-staged");
                            loop { std::thread::park(); }
                        },
                    )?;
                    connection.execute_batch("CREATE TEMP TRIGGER retain_uncommitted_main AFTER UPDATE OF status ON _uqa_mvcc_transactions WHEN NEW.status = 2 BEGIN SELECT __uqa_test_main_commit_gate(); END")?;
                    // This is the same physical commit owner used by SQLiteRecordStore. The gate runs after all records, heads and the terminal receipt are staged, while their native transaction is still open.
                    crate::mvcc::write::commit(connection, committed_id, &committed_write, store.native, &control).unwrap();
                    unreachable!("the parent must terminate the process at the physical staging barrier");
                }).unwrap();
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
    for (mode, committed_before_loss) in (0..8).flat_map(|mode| [(mode, false), (mode, true)]) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("liveness.db");
        let store = record_store(&path, mode, true);
        let control = control();
        let (pending_key, _) = record(&store, b"pending", &control);
        let (committed_key, committed_value) = record(&store, b"committed", &control);
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
        if committed_before_loss {
            peer.command("commit", "records-committed");
        } else {
            peer.command("stage-main", "main-records-staged");
        }
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
        assert!(snapshot.get(&pending_key, &control).unwrap().is_none());
        let record = snapshot.get(&committed_key, &control).unwrap();
        if committed_before_loss {
            assert!(matches!(outcome, CommitStatus::Committed(_)));
            assert_eq!(
                record.unwrap().value().map(|value| &***value),
                Some(committed_value.as_slice())
            );
        } else {
            assert_eq!(outcome, CommitStatus::Aborted);
            assert!(record.is_none());
        }
        let mut held = store.serializable_admission(&control).unwrap();
        held.graph().check_active(survivor.id()).unwrap();
        assert!(held.graph().check_active(actor(active)).is_err());
        if committed_before_loss {
            assert_eq!(
                held.graph_mut()
                    .resolve_publication(publication, CommitStatus::Unknown)
                    .unwrap(),
                outcome
            );
        }
        drop((held, survivor, later, snapshot));
        store.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}
