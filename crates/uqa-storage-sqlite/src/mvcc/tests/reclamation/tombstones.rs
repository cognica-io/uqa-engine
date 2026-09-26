//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::mvcc::{TombstoneReclamationRequest, TombstoneReclamationStep};

#[test]
fn tombstone_reclamation_preserves_absence_guards_receipts_and_epochs_across_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retired-records.db");
        {
            let connection = open(&path, mode);
            let store = SQLiteRecordStore::new(&connection).unwrap();
            uqa_storage::mvcc::verify_tombstone_reclamation(&store).unwrap();
            uqa_storage::mvcc::verify_tombstone_reclamation_pages(&store).unwrap();
            connection.with_physical(|sqlite| {
                for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                    let count: i64 = sqlite.query_row(&format!("SELECT count(*) FROM {table} WHERE key >= x'70616765732f' AND key < x'706167657330'"), [], |row| row.get(0))?;
                    assert_eq!(count, 1, "only the recreated live page remains in {table}");
                }
                Ok(())
            }).unwrap();
        }
        let connection = open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let snapshot = store.snapshot(&control()).unwrap();
        assert_eq!(snapshot.reclamation_epoch(), Some(4));
        assert!(snapshot
            .get(b"pages/000/unique-0", &control())
            .unwrap()
            .is_none());
        let prepared = prepared(b"retired/key", b"stale", &control());
        let id = store.allocate_transaction(&control()).unwrap();
        assert!(matches!(
            store.commit(id, &prepared, &control()),
            Err(CommitFailure::Rejected(
                VersionError::ReclaimedObservation { .. }
            ))
        ));
    }
}

#[test]
fn tombstone_reclamation_splits_runs_atomically_and_shares_independent_snapshot_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tombstone-runs.db");
    let connection = open(&path, 0);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let other = open(&path, 0);
    let peer = SQLiteRecordStore::new(&other).unwrap();
    let control = control();
    let keys: Vec<_> = (0..130_u64)
        .map(|number| [b"runs/".as_slice(), &number.to_be_bytes()].concat())
        .collect();
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: None,
        })
        .collect();
    let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &prepared, &control).unwrap();
    store.reclaim_versions(&control).unwrap();
    let count_runs = || {
        store
            .with(|sqlite| {
                Ok(
                    sqlite.query_row("SELECT count(*) FROM _uqa_mvcc_runs", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    assert!(count_runs() > 0);
    let request = TombstoneReclamationRequest {
        prefix: b"runs/",
        after: None,
        through: receipt.sequence,
    };
    let retained = peer.snapshot(&control).unwrap();
    assert!(matches!(
        store.reclaim_tombstones(&request, &control).unwrap(),
        TombstoneReclamationStep::Retained
    ));
    assert_eq!(
        retained
            .get(&keys[80], &control)
            .unwrap()
            .unwrap()
            .sequence(),
        receipt.sequence
    );
    drop(retained);
    store.with(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_tombstone_epoch BEFORE INSERT ON _uqa_mvcc_identifiers BEGIN SELECT RAISE(ABORT, 'injected epoch failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(store.reclaim_tombstones(&request, &control).is_err());
    let snapshot = store.snapshot(&control).unwrap();
    assert_eq!(snapshot.reclamation_epoch(), Some(0));
    for key in &keys {
        assert_eq!(
            snapshot.get(key, &control).unwrap().unwrap().sequence(),
            receipt.sequence
        );
    }
    drop(snapshot);
    store
        .with(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER reject_tombstone_epoch")?;
            Ok(())
        })
        .unwrap();
    let removed = drain_pages(&store, &request, &control);
    assert_eq!(removed, 130);
    assert_eq!(count_runs(), 0);
    assert_eq!(
        store.snapshot(&control).unwrap().reclamation_epoch(),
        Some(3)
    );
    assert_eq!(store.commit(id, &prepared, &control).unwrap(), receipt);
    store
        .with(|sqlite| {
            for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                assert_eq!(
                    sqlite.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))?,
                    0
                );
            }
            Ok(())
        })
        .unwrap();
}

fn drain_pages(
    store: &SQLiteRecordStore,
    request: &TombstoneReclamationRequest<'_>,
    control: &StorageReadControl,
) -> usize {
    let mut removed = 0;
    let mut after = None;
    loop {
        let request = TombstoneReclamationRequest {
            after: after.as_deref(),
            ..*request
        };
        match store.reclaim_tombstones(&request, control).unwrap() {
            TombstoneReclamationStep::More {
                after: cursor,
                removed: count,
            } => {
                assert_eq!(count, 64);
                removed += count;
                after = Some(cursor);
            }
            TombstoneReclamationStep::Complete { removed: count } => {
                removed += count;
                break;
            }
            TombstoneReclamationStep::Retained => panic!("all snapshots were released"),
        }
    }
    removed
}
