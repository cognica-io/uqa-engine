//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture and read failures preserve owned leases, cancellation and format admission.

use super::*;

#[test]
fn missing_or_corrupted_metadata_never_produces_a_backup_watermark() {
    for sql in [
        "DELETE FROM _uqa_mvcc_identifier_history",
        "DELETE FROM _uqa_mvcc_identifier_epochs",
        "UPDATE _uqa_mvcc_identifier_history SET watermark = x'ffffffffffffffff'",
        "UPDATE _uqa_mvcc_identifier_epochs SET head = x'ffffffffffffffff'",
        "UPDATE _uqa_mvcc_backup_metadata SET epoch = x'0000000000000000'",
    ] {
        let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
        let control = control();
        observe(&store, b"a", 10, &control);
        let snapshot = capture(&store, &control);
        observe(&store, b"a", 20, &control);
        store
            .with(|connection| {
                let _permit = schema::WritePermit::acquire(connection)?;
                connection.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        let mut called = false;
        assert!(
            snapshot
                .visit_identifier_watermarks(None, 1, &control, &mut |_, _| {
                    called = true;
                    Ok(true)
                })
                .is_err(),
            "{sql}"
        );
        assert!(!called, "{sql}");
    }
}

#[test]
fn cancelled_failed_and_independent_captures_release_their_own_allowances() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let zero = StorageReadControl::with_limit(0);
    assert!(matches!(
        store.capture_backup_snapshot(&zero),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(zero.memory().used(), 0);
    let cancelled = control();
    cancelled.cancellation().cancel();
    assert!(matches!(
        store.capture_backup_snapshot(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    let a = control();
    let b = control();
    let first = capture(&store, &a);
    let peer = SQLiteRecordStore::new(&connection.new_session()).unwrap();
    let second = capture(&peer, &b);
    assert!(a.memory().used() > 0);
    assert!(b.memory().used() > 0);
    drop(first);
    assert_eq!(a.memory().used(), 0);
    assert!(b.memory().used() > 0);
    drop(second);
    assert_eq!(b.memory().used(), 0);
}

#[test]
fn callbacks_stop_fail_and_cancel_without_retaining_a_physical_read() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    observe(&store, b"a", 1, &control);
    observe(&store, b"b", 2, &control);
    let snapshot = capture(&store, &control);
    let mut count = 0;
    snapshot
        .visit_identifier_watermarks(None, 0, &control, &mut |_, _| {
            count += 1;
            Ok(true)
        })
        .unwrap();
    assert_eq!(count, 0);
    snapshot
        .visit_identifier_watermarks(None, 2, &control, &mut |_, _| {
            count += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(matches!(
        snapshot.visit_identifier_watermarks(None, 2, &control, &mut |_, _| Err(
            VersionError::InvalidEncoding("callback failed")
        )),
        Err(VersionError::InvalidEncoding("callback failed"))
    ));
    let cancelled = super::super::control();
    assert!(matches!(
        snapshot.visit_identifier_watermarks(None, 2, &cancelled, &mut |_, _| {
            cancelled.cancellation().cancel();
            Ok(true)
        }),
        Err(VersionError::Cancelled(_))
    ));
    observe(&store, b"a", 3, &control);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"a".to_vec(), 1), (b"b".to_vec(), 2)]
    );
}

#[test]
fn large_metadata_capture_uses_fixed_retained_memory_and_bounded_windows() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let setup = control();
    for index in 0..4096_u32 {
        observe(&store, &index.to_be_bytes(), u64::from(index), &setup);
    }
    let control = StorageReadControl::with_limit(2048);
    let snapshot = capture(&store, &control);
    let retained = control.memory().used();
    assert!(retained < 2048);
    let mut after = None;
    let mut count = 0_u64;
    loop {
        let mut next = None;
        let mut window = 0;
        snapshot
            .visit_identifier_watermarks(after.as_deref(), 32, &control, &mut |key, value| {
                assert_eq!(value, Some(count));
                assert_eq!(key, (count as u32).to_be_bytes());
                count += 1;
                window += 1;
                next = Some(key.to_vec());
                Ok(true)
            })
            .unwrap();
        assert!(window <= 32);
        assert_eq!(control.memory().used(), retained);
        if next.is_none() {
            break;
        }
        after = next;
    }
    assert_eq!(count, 4096);
    assert!(control.memory().peak() <= 2048);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_capture_commit_preserves_the_boundary_and_later_capture_remains_usable() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    observe(&store, b"a", 8, &control);
    store.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_backup_cut AFTER UPDATE ON _uqa_mvcc_backup_metadata BEGIN SELECT RAISE(ABORT, 'injected capture failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(store.capture_backup_snapshot(&control).is_err());
    assert_eq!(control.memory().used(), 0);
    store
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER reject_backup_cut")?;
            Ok(())
        })
        .unwrap();
    let snapshot = capture(&store, &control);
    assert_eq!(snapshot.metadata_epoch(), MetadataEpoch::INITIAL);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"a".to_vec(), 8)]
    );
}

#[test]
fn cold_predecessor_upgrade_retains_watermarks_and_epoch_exhaustion_cannot_wrap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("previous.db");
    let control = control();
    let identity = {
        let store = SQLiteRecordStore::new(&ManagedConnection::open(&path).unwrap()).unwrap();
        observe(&store, b"old", u64::MAX, &control);
        super::super::downgrade_record_format(&store, 67);
        store.identity
    };
    let store = SQLiteRecordStore::new(&ManagedConnection::open(&path).unwrap()).unwrap();
    assert_eq!(store.identity, identity);
    let snapshot = capture(&store, &control);
    assert_eq!(snapshot.metadata_epoch(), MetadataEpoch::INITIAL);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"old".to_vec(), u64::MAX)]
    );
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute_batch(
                "UPDATE _uqa_mvcc_backup_metadata SET epoch = x'ffffffffffffffff'",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        store.capture_backup_snapshot(&control),
        Err(VersionError::InvalidEncoding("metadata epoch exhausted"))
    ));
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"old".to_vec(), u64::MAX)]
    );
}
