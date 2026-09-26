//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::vector_index::VectorIndex;

fn catalog_with_docs(connection: &ManagedConnection) -> Catalog {
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&uqa_storage::TableSchema {
            relation: uqa_storage::RelationIdentity::new("public", "docs"),
            security: uqa_storage::RelationSecurityRow::legacy("owner"),
            object_id: [0; 16],
            storage_generation: [0; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec![],
            vector_fields: vec![],
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        })
        .unwrap();
    catalog
}

#[test]
fn native_diskann_canonical_origins_follow_catalog_renames_cleanup_and_undo() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let catalog = catalog_with_docs(&connection);
    let original = canonical(&connection, "public.docs", "before", 2);
    let version = original.replace(1, &[vec![1.0, -0.0]], &control).unwrap();
    original.replace(2, &[], &control).unwrap();
    let retained = original.retain(&control).unwrap();
    catalog
        .rename_column_data("public.docs", "before", "after")
        .unwrap();
    let field = canonical(&connection, "public.docs", "after", 2);
    assert_change(&field.retain(&control).unwrap(), 1, version, &control);
    assert_eq!(
        field.retain(&control).unwrap().origin(1, &control).unwrap(),
        Some(version)
    );
    assert!(original
        .retain(&control)
        .unwrap()
        .origin(2, &control)
        .unwrap()
        .is_none());
    catalog
        .rename_table_data("public.docs", "public.renamed")
        .unwrap();
    let renamed = canonical(&connection, "public.renamed", "after", 2);
    assert_change(&renamed.retain(&control).unwrap(), 1, version, &control);
    assert_eq!(
        renamed
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .unwrap(),
        Some(version)
    );
    assert!(field
        .retain(&control)
        .unwrap()
        .origin(2, &control)
        .unwrap()
        .is_none());
    let destination = canonical(&connection, "public.destination", "after", 2);
    destination.replace(2, &[], &control).unwrap();
    assert!(catalog
        .rename_table_data("public.renamed", "public.destination")
        .is_err());
    connection.begin_transaction().unwrap();
    catalog.drop_column_data("public.renamed", "after").unwrap();
    assert!(renamed
        .retain(&control)
        .unwrap()
        .origin(1, &control)
        .unwrap()
        .is_none());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        renamed
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .unwrap(),
        Some(version)
    );
    catalog.drop_column_data("public.renamed", "after").unwrap();
    assert_eq!(change_count(&connection), 1);
    assert!(renamed
        .retain(&control)
        .unwrap()
        .origin(2, &control)
        .unwrap()
        .is_none());
    for drop_table in [false, true] {
        renamed.replace(1, &[vec![2.0, 3.0]], &control).unwrap();
        renamed.replace(2, &[], &control).unwrap();
        if drop_table {
            catalog.drop_table_and_data("public.renamed").unwrap();
        } else {
            catalog.purge_table_data("public.renamed").unwrap();
        }
        let read = renamed.retain(&control).unwrap();
        assert_eq!(change_count(&connection), 1);
        assert!(read.origin(1, &control).unwrap().is_none());
        assert!(read.origin(2, &control).unwrap().is_none());
    }
    let recreated = renamed.replace(1, &[vec![4.0, 5.0]], &control).unwrap();
    assert_ne!(recreated, version);
    assert_tensor(&retained, 1, &[vec![1.0, -0.0]], &control);
    assert_change(&retained, 1, version, &control);
    assert!(retained.origin(2, &control).unwrap().is_some());
}

#[test]
fn native_diskann_canonical_origins_are_invalidated_by_each_legacy_vector_owner() {
    for kind in 0..3 {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        let source = canonical(&connection, "docs", "embedding", 2);
        let mut legacy: Box<dyn VectorIndex> = match kind {
            0 => Box::new(crate::SQLiteVectorIndex::new(
                connection.clone(),
                "docs",
                "embedding",
                2,
            )),
            1 => Box::new(crate::SQLiteIVFIndex::new(
                connection.clone(),
                "docs",
                "embedding",
                2,
            )),
            _ => Box::new(crate::SQLiteHNSWIndex::new(
                connection.clone(),
                "docs",
                "embedding",
                2,
            )),
        };
        legacy.add(1, vec![1.0, 0.0]).unwrap();
        source.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let retained = source.retain(&control).unwrap();
        legacy.add(1, vec![0.0, 1.0]).unwrap();
        assert!(source
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .is_err());
        assert_tensor(&retained, 1, &[vec![1.0, 0.0]], &control);
        legacy.delete(1).unwrap();
        assert!(source
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .unwrap()
            .is_none());
        source.replace(2, &[], &control).unwrap();
        legacy.clear().unwrap();
        assert_eq!(change_count(&connection), 0);
        assert!(source
            .retain(&control)
            .unwrap()
            .origin(2, &control)
            .unwrap()
            .is_none());
    }
}

#[test]
fn native_diskann_origin_scope_cleans_failed_evaluation_and_preserves_explicit_transactions() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    for explicit in [false, true] {
        for unwind in [false, true] {
            if explicit {
                connection.begin_transaction().unwrap();
            }
            let mut origin = None;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let result: crate::Result<()> =
                    connection.with_native_versioned_write(|current, snapshot, batch| {
                        origin = Some(current);
                        snapshot.ensure_table_owner("never-published", batch)?;
                        assert!(!unwind, "injected native origin evaluation panic");
                        Err(crate::SQLiteError::StorageBackend(
                            "injected native origin failure".into(),
                        ))
                    });
                assert!(result.is_err());
            }));
            assert_eq!(result.is_err(), unwind);
            assert_eq!(connection.in_transaction(), explicit);
            assert!(connection
                .native_snapshot()
                .unwrap()
                .unwrap()
                .table_owner("never-published")
                .unwrap()
                .is_none());
            if explicit {
                connection.rollback_transaction().unwrap();
            }
            let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            assert_eq!(
                records
                    .commit_status(origin.unwrap().transaction(), &control)
                    .unwrap(),
                CommitStatus::Aborted
            );
        }
    }
    connection.begin_record_read().unwrap();
    let mut called = false;
    assert!(connection
        .with_native_versioned_write(|_, _, _| {
            called = true;
            Ok(())
        })
        .is_err());
    assert!(!called);
    assert!(!connection.transaction_has_written().unwrap());
    connection.commit_transaction().unwrap();
}

#[test]
fn native_diskann_origin_scope_aborts_cancelled_evaluation_with_independent_cleanup() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let mut origin = None;
    let result: crate::Result<()> =
        connection.with_native_versioned_write(|current, snapshot, batch| {
            origin = Some(current);
            snapshot.ensure_table_owner("cancelled", batch)?;
            connection.write_cancellation().cancel();
            Err(crate::SQLiteError::StorageBackend(
                "cancelled origin evaluation".into(),
            ))
        });
    assert!(result.is_err());
    assert!(!connection.in_transaction());
    let peer = connection.new_session();
    assert!(peer
        .native_snapshot()
        .unwrap()
        .unwrap()
        .table_owner("cancelled")
        .unwrap()
        .is_none());
    let records = SQLiteRecordStore::for_native(&peer, &control).unwrap();
    assert_eq!(
        records
            .commit_status(origin.unwrap().transaction(), &control)
            .unwrap(),
        CommitStatus::Aborted
    );
}

#[test]
fn native_diskann_origin_cleanup_failures_retain_an_abort_only_attempt() {
    for status in [1, 3] {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        connection.with_physical(|sqlite| {
            sqlite.execute_batch(&format!("CREATE TRIGGER reject_origin_cleanup BEFORE UPDATE ON _uqa_mvcc_transactions WHEN NEW.status={status} BEGIN SELECT RAISE(ABORT, 'injected origin cleanup failure'); END;"))?;
            Ok(())
        }).unwrap();
        let mut origin = None;
        let result: crate::Result<()> =
            connection.with_native_versioned_write(|current, snapshot, batch| {
                origin = Some(current);
                snapshot.ensure_table_owner("failed-cleanup", batch)?;
                Err(crate::SQLiteError::StorageBackend(
                    "injected evaluation failure".into(),
                ))
            });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("injected origin cleanup failure"));
        assert!(connection.in_transaction());
        assert!(connection.commit_transaction().is_err());
        let mut replayed = false;
        assert!(connection
            .with_native_versioned_write(|_, _, _| {
                replayed = true;
                Ok(())
            })
            .is_err());
        assert!(!replayed);
        let peer = connection.new_session();
        assert!(peer
            .native_snapshot()
            .unwrap()
            .unwrap()
            .table_owner("failed-cleanup")
            .unwrap()
            .is_none());
        peer.with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER reject_origin_cleanup")?;
            Ok(())
        })
        .unwrap();
        connection.rollback_transaction().unwrap();
        assert!(!connection.in_transaction());
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(
            records
                .commit_status(origin.unwrap().transaction(), &control)
                .unwrap(),
            CommitStatus::Aborted
        );
        connection
            .with_native_versioned_write(|current, _, _| {
                assert_ne!(current.transaction(), origin.unwrap().transaction());
                Ok(())
            })
            .unwrap();
    }
}
