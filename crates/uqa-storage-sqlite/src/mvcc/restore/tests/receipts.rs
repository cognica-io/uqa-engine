//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore preserves configured capacity and safely upgrades interrupted predecessor intents.

use super::*;

#[test]
fn restored_receipt_capacity_survives_retry_and_live_managed_owners_exclude_restore() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt-restore.db");
            let backup = seed(&path, mode, native);
            let control = control();
            let connection = open(&path, mode).unwrap();
            let store = records(&connection, native);
            let limit = backup.pending.allocation() + 1;
            store.set_receipt_retention_limit(limit, &control).unwrap();
            let owner = store.allocate_managed_transaction(&control).unwrap();
            drop(store);
            drop(connection);
            assert!(matches!(
                restored(&path, mode, backup.request, &control),
                Err(SQLiteError::DatabaseRestoreBusy)
            ));
            drop(owner);
            let connection = restored(&path, mode, backup.request, &control).unwrap();
            verify(&connection, &backup, native);
            let store = records(&connection, native);
            for _ in 0..limit {
                store.allocate_transaction(&control).unwrap();
            }
            assert!(matches!(
                store.allocate_transaction(&control),
                Err(VersionError::ReceiptRetentionExhausted { limit: actual }) if actual == limit
            ));
            drop(store);
            drop(connection);
            let connection = restored(&path, mode, backup.request, &control).unwrap();
            assert!(matches!(
                records(&connection, native).allocate_transaction(&control),
                Err(VersionError::ReceiptRetentionExhausted { limit: actual }) if actual == limit
            ));
        }
    }
}

#[test]
fn interrupted_predecessor_restores_upgrade_without_losing_the_original_intent() {
    for mode in 0..4 {
        for native in [false, true] {
            for (coordinator_published, predecessor) in [false, true]
                .into_iter()
                .flat_map(|published| [43, 44, 45, 46].map(|format| (published, format)))
            {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("old-pending-restore.db");
                let backup = seed(&path, mode, native);
                let control = control();
                {
                    let connection = open(&path, mode).unwrap();
                    let store = records(&connection, native);
                    if coordinator_published {
                        store
                            .serializable_admission(&control)
                            .unwrap()
                            .publish_restored(backup.request, &control)
                            .unwrap();
                    }
                    if predecessor >= 44 {
                        store.set_receipt_retention_limit(123, &control).unwrap();
                    }
                    crate::mvcc::tests::downgrade_record_format(&store, predecessor);
                    store
                        .with(|sqlite| {
                            let _permit = schema::WritePermit::acquire(sqlite)?;
                            sqlite.execute(
                                "UPDATE _uqa_mvcc_metadata SET restore_target = ?1",
                                [backup.request.target().as_bytes().as_slice()],
                            )?;
                            Ok(())
                        })
                        .unwrap();
                }
                assert!(matches!(
                    open(&path, mode),
                    Err(SQLiteError::DatabaseRestoreIncomplete)
                ));
                let different = DatabaseRestore::new(backup.request.source()).unwrap();
                assert!(restored(&path, mode, different, &control).is_err());
                let _injection = inject(Boundary::IntentPublished, || {
                    Err(SQLiteError::Io(std::io::Error::other("upgrade reply lost")).into())
                });
                assert!(restored(&path, mode, backup.request, &control).is_err());
                assert!(matches!(
                    open(&path, mode),
                    Err(SQLiteError::DatabaseRestoreIncomplete)
                ));
                let connection = restored(&path, mode, backup.request, &control).unwrap();
                verify(&connection, &backup, native);
                connection
                    .record_connection()
                    .with(|sqlite| {
                        let (format, limit, pending): (i64, i64, Option<Vec<u8>>) = sqlite
                            .query_row(
                            "SELECT format, receipt_limit, restore_target FROM _uqa_mvcc_metadata",
                            [],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )?;
                        assert_eq!(format, 47);
                        assert_eq!(
                            limit,
                            if predecessor >= 44 {
                                123
                            } else {
                                i64::try_from(uqa_storage::mvcc::DEFAULT_RECEIPT_RETENTION_LIMIT)
                                    .unwrap()
                            }
                        );
                        assert_eq!(pending, None);
                        Ok(())
                    })
                    .unwrap();
            }
        }
    }
}
