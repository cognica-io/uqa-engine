//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn interrupted_restore_boundaries_resume_only_the_original_request() {
    for mode in 0..4 {
        for native in [false, true] {
            for at in [
                Boundary::IntentPublished,
                Boundary::CoordinatorPublished,
                Boundary::Completed,
            ] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("interrupted.db");
                let backup = seed(&path, mode, native);
                let control = control();
                let _injection = inject(at, || {
                    Err(
                        SQLiteError::Io(std::io::Error::other("restore publication outcome lost"))
                            .into(),
                    )
                });
                assert!(restored(&path, mode, backup.request, &control).is_err());
                if at != Boundary::Completed {
                    assert!(matches!(
                        open(&path, mode),
                        Err(SQLiteError::DatabaseRestoreIncomplete)
                    ));
                    let different = DatabaseRestore::new(backup.request.source()).unwrap();
                    assert!(restored(&path, mode, different, &control).is_err());
                }
                let connection = restored(&path, mode, backup.request, &control).unwrap();
                verify(&connection, &backup, native);
            }
        }
    }
}

#[test]
fn restore_resource_failures_keep_typed_errors_and_resumable_history() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("resource.db");
        let backup = seed(&path, mode, false);
        let original = std::fs::read(&path).unwrap();
        assert!(matches!(
            restored(
                &path,
                mode,
                backup.request,
                &StorageReadControl::with_limit(0)
            ),
            Err(SQLiteError::Memory(_))
        ));
        let cancelled = control();
        cancelled.cancellation().cancel();
        assert!(matches!(
            restored(&path, mode, backup.request, &cancelled),
            Err(SQLiteError::Cancelled(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let controlled = control();
        let token = controlled.cancellation().clone();
        let _injection = inject(Boundary::IntentPublished, move || {
            token.cancel();
            Ok(())
        });
        assert!(matches!(
            restored(&path, mode, backup.request, &controlled),
            Err(SQLiteError::Cancelled(_))
        ));
        assert_eq!(controlled.memory().used(), 0);
        assert!(matches!(
            open(&path, mode),
            Err(SQLiteError::DatabaseRestoreIncomplete)
        ));
        let connection = restored(&path, mode, backup.request, &control()).unwrap();
        verify(&connection, &backup, false);
    }
}

#[test]
fn restoration_rejects_missing_empty_uninitialized_and_unrelated_inputs_without_main_changes() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.db");
    let request = DatabaseRestore::new(DatabaseId::from_bytes([1; 16])).unwrap();
    assert!(restored(&missing, 0, request, &control()).is_err());
    assert!(!missing.exists());
    let empty = directory.path().join("empty.db");
    std::fs::write(&empty, []).unwrap();
    assert!(restored(&empty, 0, request, &control()).is_err());
    assert_eq!(empty.metadata().unwrap().len(), 0);
    for mode in 0..4 {
        let path = directory.path().join(format!("unrelated-{mode}.db"));
        {
            let connection = open(&path, mode).unwrap();
            connection.with(|sqlite| {
                sqlite.execute_batch("CREATE TABLE user_data (value TEXT); INSERT INTO user_data VALUES ('preserve')")?;
                Ok(())
            }).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert!(restored(&path, mode, request, &control()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let path = directory.path().join(format!("foreign-{mode}.db"));
        seed(&path, mode, true);
        let before = std::fs::read(&path).unwrap();
        assert!(restored(&path, mode, request, &control()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn malformed_main_and_auxiliary_components_fail_before_durable_restore_intent() {
    for mode in 0..4 {
        for damage in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("malformed.db");
            let backup = seed(&path, mode, damage == 0);
            {
                let connection = open(&path, mode).unwrap();
                match damage {
                    0 => connection
                        .record_connection()
                        .with(|sqlite| {
                            sqlite.execute_batch("DROP TRIGGER _uqa_mvcc_heads_UPDATE_guard")?;
                            Ok(())
                        })
                        .unwrap(),
                    1..=3 => {
                        let auxiliary = connection.open_serializable_connection().unwrap();
                        auxiliary.with(|sqlite| {
                            sqlite.execute_batch(match damage {
                                1 => "DROP TABLE _uqa_serializable_records",
                                2 => "UPDATE _uqa_serializable_state SET database_id = zeroblob(16)",
                                _ => "UPDATE _uqa_serializable_records SET value = X'00'",
                            })?;
                            Ok(())
                        }).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            let before = std::fs::read(&path).unwrap();
            assert!(
                restored(&path, mode, backup.request, &control()).is_err(),
                "mode {mode}, damage {damage}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            // No pending marker was published; physical maintenance remains available.
            drop(open(&path, mode).unwrap());
        }
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn legacy_detached_snapshot_and_ssi_leases_exclude_restore() {
    for serializable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-owner.db");
        let backup = seed(&path, 0, false);
        let control = control();
        let file = if serializable {
            let graph = uqa_storage::mvcc::SerializableGraph::new(
                backup.request.source(),
                backup.coordinator,
                control.memory(),
            )
            .unwrap();
            crate::mvcc::serializable::lease_file(&path, &graph).unwrap()
        } else {
            crate::mvcc::retention::lease_file(&path, backup.request.source()).unwrap()
        };
        let admission = file.admit(&control).unwrap();
        let retained = file.retain(1, &control).unwrap();
        drop(admission);
        assert!(matches!(
            restored(&path, 0, backup.request, &control),
            Err(SQLiteError::DatabaseRestoreBusy)
        ));
        drop(retained);
        drop(file);
        let connection = restored(&path, 0, backup.request, &control).unwrap();
        verify(&connection, &backup, false);
    }
}

#[test]
fn encrypted_compressed_restore_checks_trusted_anchors_and_retains_native_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("anchored.db");
    let backup = seed(&path, 3, true);
    let original = crate::read_authenticated_anchor(&path, "closed-backup").unwrap();
    let mut wrong = original;
    wrong.state_tag[0] ^= 1;
    let restore = |request, anchor| {
        ManagedConnection::open_compressed_encrypted_restored_with_anchor(
            &path,
            "closed-backup",
            SQLiteCompressionOptions::default(),
            anchor,
            request,
            &control(),
        )
    };
    assert!(restore(backup.request, wrong).is_err());
    assert_eq!(
        crate::read_authenticated_anchor(&path, "closed-backup").unwrap(),
        original
    );
    let connection = restore(backup.request, original).unwrap();
    verify(&connection, &backup, true);
    drop(connection);
    let current = crate::read_authenticated_anchor(&path, "closed-backup").unwrap();
    assert_ne!(current, original);
    assert!(restore(backup.request, original).is_err());
    drop(restore(backup.request, current).unwrap());
    let current = crate::read_authenticated_anchor(&path, "closed-backup").unwrap();
    let again = DatabaseRestore::new(backup.request.target()).unwrap();
    let connection = restore(again, current).unwrap();
    let store = records(&connection, true);
    assert_eq!(store.database_id(), again.target());
    assert_eq!(store.native_namespace(), backup.namespace);
    assert_eq!(
        &***store
            .snapshot(&control())
            .unwrap()
            .get(&backup.key, &control())
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        &backup.value
    );
}
