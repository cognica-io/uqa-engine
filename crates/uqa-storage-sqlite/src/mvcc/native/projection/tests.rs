//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! New records have no original rows after revision validation.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteRecordStore};
use uqa_storage::mvcc::{PrivateRecordChanges, RecordWrite, VersionedPersistence};

mod occurrences;

#[test]
fn unchanged_native_btree_writes_keep_history_conflicts_and_reopen() {
    use crate::mvcc::native::tests::materialization::{initialize, records, replace, with};
    use uqa_storage::{
        mvcc::{VersionedKeyValueStore, VersionedSessionOptions},
        KeyValueStore,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unchanged-index.db");
    let control = StorageReadControl::with_limit(1 << 20);
    let (transaction, receipt) = {
        let connection = ManagedConnection::open(&path).unwrap();
        initialize(&connection);
        with(&connection, |sqlite| {
            sqlite.execute_batch("INSERT INTO _btree_indexes VALUES('public.docs','n'); INSERT INTO _btree_index_entries VALUES('public.docs','n',1,'{\"type\":\"Int\",\"value\":1}');")?;
            Ok(())
        });
        let store = Arc::new(SQLiteRecordStore::for_native(&connection, &control).unwrap());
        let original = records(&connection, &store, Family::BtreeIndexEntries, &control);
        assert_eq!(original.len(), 1);
        let original = &original[0];
        let retained = store.snapshot(&control).unwrap();
        let old = retained.get(original.key(), &control).unwrap().unwrap();
        let writes = [original.write(Some(old.sequence()))];
        let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
        let transaction = store.allocate_transaction(&control).unwrap();
        let receipt = store.commit(transaction, &prepared, &control).unwrap();
        assert_eq!(
            store.commit(transaction, &prepared, &control).unwrap(),
            receipt
        );
        let latest = store.snapshot(&control).unwrap();
        let current = latest.get(original.key(), &control).unwrap().unwrap();
        assert_eq!(current.sequence(), receipt.sequence);
        assert!(current.sequence() > old.sequence());
        assert_eq!(&***current.value().unwrap(), original.row());
        assert_eq!(
            &***retained
                .get(original.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            original.row()
        );

        let first =
            VersionedKeyValueStore::new(store.clone(), None, VersionedSessionOptions::default());
        let second =
            VersionedKeyValueStore::new(store.clone(), None, VersionedSessionOptions::default());
        first.begin_transaction().unwrap();
        second.begin_transaction().unwrap();
        first.put(original.key(), original.row()).unwrap();
        let changed = replace(
            original,
            3,
            ValueRef::Text(br#"{"type":"Int","value":2}"#),
            &control,
        );
        second.put(changed.key(), changed.row()).unwrap();
        second.commit_transaction().unwrap();
        let error = first.commit_transaction().unwrap_err();
        assert_write_conflict(&error);
        if first.in_transaction() {
            first.rollback_transaction().unwrap();
        }
        with(&connection, |sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT value_json FROM _btree_index_entries WHERE doc_id=1",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                r#"{"type":"Int","value":2}"#
            );
            assert_eq!(sqlite.query_row("SELECT count(*) FROM _btree_equality_keys WHERE doc_id=1 AND equality_key=__uqa_btree_equal_v1('{\"type\":\"Int\",\"value\":2}')",[],|row|row.get::<_,i64>(0))?,1);
            Ok(())
        });
        (transaction, receipt)
    };
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(
        store.commit_status(transaction, &control).unwrap(),
        uqa_storage::mvcc::CommitStatus::Committed(receipt)
    );
    with(&connection, |sqlite| {
        assert_eq!(
            sqlite.query_row(
                "SELECT value_json FROM _btree_index_entries WHERE doc_id=1",
                [],
                |row| row.get::<_, String>(0)
            )?,
            r#"{"type":"Int","value":2}"#
        );
        Ok(())
    });
}

fn assert_write_conflict(error: &uqa_storage::StorageBackendError) {
    use uqa_storage::mvcc::CommitFailure;
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    let mut conflict = false;
    while let Some(error) = cause {
        conflict |= matches!(
            error.downcast_ref::<VersionError>(),
            Some(VersionError::WriteConflict { .. })
        ) || matches!(
            error.downcast_ref::<CommitFailure>(),
            Some(CommitFailure::Rejected(VersionError::WriteConflict { .. }))
        );
        cause = error.source();
    }
    assert!(
        conflict,
        "unchanged record must retain its write conflict: {error:?}"
    );
}

#[test]
fn data_commit_evidence_matches_the_exact_receipt_and_rejects_definition_changes() {
    use crate::mvcc::native::tests::materialization::{initialize, records, replace};
    use uqa_storage::{
        mvcc::{VersionedKeyValueStore, VersionedSessionOptions},
        KeyValueStore,
    };

    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(4 << 20);
    let store = Arc::new(SQLiteRecordStore::for_native(&connection, &control).unwrap());
    let original = records(&connection, &store, Family::Documents, &control);
    let updated = replace(&original[0], 2, ValueRef::Text(b"{\"n\":99}"), &control);
    let session =
        VersionedKeyValueStore::new(store.clone(), None, VersionedSessionOptions::default());
    session.put(updated.key(), updated.row()).unwrap();
    let receipt = session.completed_commit().unwrap();
    assert!(store.commit_preserves_catalog_definitions(receipt));
    let reads = crate::mvcc::RECORD_READS.with(std::cell::Cell::get);
    let revision = session.committed_data_revision().unwrap();
    assert_eq!(revision.change_version, receipt.sequence.as_u64());
    assert_eq!(crate::mvcc::RECORD_READS.with(std::cell::Cell::get), reads);
    let mut wrong = receipt;
    wrong.fingerprint[0] ^= 1;
    assert!(!store.commit_preserves_catalog_definitions(wrong));

    let metadata = NativeRecord::encode(
        Family::Metadata,
        NativeRecordOwner::Database(store.native_namespace().unwrap()),
        &[
            ValueRef::Text(b"definition_probe"),
            ValueRef::Text(b"changed"),
        ],
        &control,
    )
    .unwrap();
    session.put(metadata.key(), metadata.row()).unwrap();
    assert!(!store.commit_preserves_catalog_definitions(session.completed_commit().unwrap()));
    assert!(session.committed_data_revision().is_none());
    session.begin_read_transaction().unwrap();
    assert!(session.completed_commit().is_none());
    session.rollback_transaction().unwrap();
}

#[test]
fn validated_new_native_records_do_not_read_nonexistent_originals() {
    for count in [32_i64, 128] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        let control = StorageReadControl::with_limit(4 << 20);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let owner = NativeRecordOwner::Object {
            identity: [1; 16],
            generation: [2; 16],
        };
        let records: Vec<_> = (1..=count)
            .map(|id| {
                NativeRecord::encode(
                    Family::Documents,
                    owner,
                    &[
                        ValueRef::Text(b"docs"),
                        ValueRef::Integer(id),
                        ValueRef::Text(b"{}"),
                        ValueRef::Null,
                    ],
                    &control,
                )
                .unwrap()
            })
            .collect();
        let writes: Vec<_> = records.iter().map(|record| record.write(None)).collect();
        let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
        connection
            .with_physical(|sqlite| {
                prepared
                    .validate(&control, |key| {
                        crate::mvcc::codec::head(sqlite, key).map_err(Error::into_version)
                    })
                    .unwrap();
                let steps = Arc::new(AtomicUsize::new(0));
                let counter = Arc::clone(&steps);
                sqlite.progress_handler(
                    1,
                    Some(move || {
                        counter.fetch_add(1, Ordering::Relaxed);
                        false
                    }),
                )?;
                seed_originals(sqlite, store.database_id(), &prepared, &control).unwrap();
                sqlite.progress_handler(0, None::<fn() -> bool>)?;
                assert_eq!(
                    steps.load(Ordering::Relaxed),
                    0,
                    "{count} validated new records must not query absent originals"
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn spilled_native_publication_preserves_mixed_writes_history_and_retry() {
    use crate::mvcc::native::tests::materialization::{initialize, records, replace, with};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("spilled-publication.db");
    let control = StorageReadControl::with_limit(4 << 20);
    let (transaction, receipt) = {
        let connection = ManagedConnection::open(&path).unwrap();
        initialize(&connection);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let original = records(&connection, &store, Family::Documents, &control);
        let owner = NativeRecordIdentity::decode(original[0].key())
            .unwrap()
            .owner();
        let retained = store.snapshot(&control).unwrap();
        let budget = uqa_core::memory::MemoryBudget::new(128 << 10);
        let private = PrivateRecordChanges::new(&budget);
        let updated = replace(&original[0], 2, ValueRef::Text(b"{\"n\":99}"), &control);
        private
            .apply(
                &[
                    updated.write(Some(CommitSequence::from_u64(1))),
                    RecordWrite {
                        key: original[1].key(),
                        expected: Some(CommitSequence::from_u64(1)),
                        value: None,
                    },
                ],
                &control,
            )
            .unwrap();
        let body = format!("{{\"text\":\"{}\"}}", "x".repeat(8192));
        for id in 3..=130 {
            let record = NativeRecord::encode(
                Family::Documents,
                owner,
                &[
                    ValueRef::Text(b"public.docs"),
                    ValueRef::Integer(id),
                    ValueRef::Text(body.as_bytes()),
                    ValueRef::Null,
                ],
                &control,
            )
            .unwrap();
            private.apply(&[record.write(None)], &control).unwrap();
        }
        let prepared = private.prepare(&control).unwrap();
        assert!(prepared.resident().is_none());
        let transaction = store.allocate_transaction(&control).unwrap();
        let receipt = store.commit(transaction, &prepared, &control).unwrap();
        assert_eq!(
            store.commit(transaction, &prepared, &control).unwrap(),
            receipt
        );
        for record in &original {
            assert_eq!(
                &***retained
                    .get(record.key(), &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                record.row()
            );
        }
        with(&connection, |sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT count(*) FROM _documents", [], |row| row
                    .get::<_, i64>(0))?,
                129
            );
            assert_eq!(
                sqlite.query_row("SELECT body FROM _documents WHERE doc_id = 1", [], |row| {
                    row.get::<_, String>(0)
                })?,
                "{\"n\":99}"
            );
            Ok(())
        });
        (transaction, receipt)
    };
    let reopened = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::for_native(&reopened, &control).unwrap();
    assert_eq!(
        store.commit_status(transaction, &control).unwrap(),
        uqa_storage::mvcc::CommitStatus::Committed(receipt)
    );
    with(&reopened, |sqlite| {
        assert_eq!(
            sqlite.query_row("SELECT count(*) FROM _documents", [], |row| row
                .get::<_, i64>(0))?,
            129
        );
        Ok(())
    });
}

#[test]
fn native_validation_queue_does_not_duplicate_large_prepared_values() {
    use crate::mvcc::native::tests::materialization::{initialize, records, replace, with};
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(4 << 20);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records(&connection, &store, Family::Documents, &control);
    let payload = format!("{{\"text\":\"{}\"}}", "x".repeat(128 << 10));
    let changed = replace(
        &original[0],
        2,
        ValueRef::Text(payload.as_bytes()),
        &control,
    );
    let retained = store.snapshot(&control).unwrap();
    let old = retained.get(changed.key(), &control).unwrap().unwrap();
    let prepared =
        PreparedRecordCommit::new(&[changed.write(Some(old.sequence()))], &control).unwrap();
    let copies = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&copies);
    with(&connection, |sqlite| {
        sqlite.create_scalar_function(
            "observe_validation_bytes",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |context| {
                observed.fetch_add(context.get::<i64>(0)? as usize, Ordering::Relaxed);
                Ok(0)
            },
        )?;
        sqlite.execute_batch("CREATE TEMP TRIGGER observe_validation AFTER INSERT ON _uqa_mvcc_native_expected BEGIN SELECT observe_validation_bytes(coalesce(length(NEW.new_value), 0)); END;")?;
        Ok(())
    });
    let id = store.allocate_transaction(&control).unwrap();
    store.commit(id, &prepared, &control).unwrap();
    assert_eq!(copies.load(Ordering::Relaxed), 0);
    assert_eq!(
        &***retained
            .get(changed.key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        original[0].row()
    );
    let latest = store.snapshot(&control).unwrap();
    assert_eq!(
        &***latest
            .get(changed.key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        changed.row()
    );
    with(&connection, |sqlite| {
        assert_eq!(
            sqlite.query_row(
                "SELECT body FROM _documents WHERE table_name='public.docs' AND doc_id=1",
                [],
                |row| row.get::<_, String>(0)
            )?,
            payload
        );
        Ok(())
    });
}
