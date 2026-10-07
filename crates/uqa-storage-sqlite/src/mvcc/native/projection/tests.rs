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
