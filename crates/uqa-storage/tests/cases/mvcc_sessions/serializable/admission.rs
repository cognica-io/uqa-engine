//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic admission schedules separate physical session permissions from logical SSI characteristics.

use std::sync::mpsc;
use std::time::Duration;

use super::*;

const SAFE_READER: SerializableSnapshotOptions = SerializableSnapshotOptions {
    read_only: true,
    deferrable: true,
};

#[test]
fn safe_snapshot_preserves_savepoints_retained_readers_and_original_lifetime() {
    let persistence = Persistence::new();
    let reader = persistence.session(1 << 20);
    reader.begin_upgradeable_transaction().unwrap();
    reader.savepoint("before_snapshot").unwrap();
    let context = reader
        .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| capture())
        .unwrap();
    assert_eq!(
        context
            .safe_snapshot(&StorageReadControl::with_limit(1 << 20))
            .unwrap(),
        SafeSnapshot::Safe
    );
    assert!(reader
        .observe_serializable_write(predicate(b"forbidden"))
        .is_err());
    let checkpoint = persistence.serializable.lock().checkpoint.clone();
    read(&context, b"absent");
    // A safe reader validates lifetime without retaining predicate keys.
    assert_eq!(persistence.serializable.lock().checkpoint, checkpoint);
    reader.rollback_to_savepoint("before_snapshot").unwrap();
    let same = reader
        .establish_serializable_snapshot_with(SAFE_READER, &mut |_| {
            panic!("an admitted transaction must not capture a new snapshot")
        })
        .unwrap();
    assert_eq!(context.id(), same.id());
    let retained = reader
        .open_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    let retained = retained
        .serializable_session()
        .unwrap()
        .serializable_read_context()
        .unwrap()
        .unwrap();
    assert_eq!(context.id(), retained.id());
    reader.commit_transaction().unwrap();
    assert!(matches!(
        retained.observe_read(
            predicate(b"after_commit"),
            &StorageReadControl::with_limit(1 << 20)
        ),
        Err(VersionError::TransactionFinished)
    ));
    assert_eq!(persistence.state.lock().next, 0);
}

#[test]
fn deferrable_read_write_and_nondeferrable_read_only_do_not_wait() {
    for options in [
        SerializableSnapshotOptions {
            read_only: false,
            deferrable: true,
        },
        SerializableSnapshotOptions {
            read_only: true,
            deferrable: false,
        },
    ] {
        let persistence = Persistence::new();
        let (writer, _) = start(&persistence, false);
        let session = persistence.session(1 << 20);
        session.begin_upgradeable_transaction().unwrap();
        let context = session
            .establish_serializable_snapshot_with(options, &mut |capture| capture())
            .unwrap();
        if options.read_only {
            assert_eq!(
                context
                    .safe_snapshot(&StorageReadControl::with_limit(1 << 20))
                    .unwrap(),
                SafeSnapshot::Pending
            );
        } else {
            write(&session, b"own_write");
        }
        session.commit_transaction().unwrap();
        writer.rollback_transaction().unwrap();
    }
}

#[test]
fn safe_wait_releases_capture_guards_and_ignores_writers_admitted_later() {
    let persistence = Persistence::new();
    let (writer, _) = start(&persistence, false);
    write(&writer, b"before_reader");
    let reader = persistence.session(1 << 20);
    reader.begin_upgradeable_transaction().unwrap();
    let cancellation = reader.write_cancellation().unwrap();
    let capture_gate = Mutex::new(());
    let (notify_capture, captures) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let context =
                reader.establish_serializable_snapshot_with(SAFE_READER, &mut |capture| {
                    let _guard = capture_gate.lock();
                    capture()?;
                    notify_capture.send(()).unwrap();
                    Ok(())
                });
            finished.send(context).unwrap();
        });
        let capture = captures.recv_timeout(Duration::from_secs(10));
        if capture.is_err() {
            cancellation.cancel();
        }
        capture.unwrap();
        // Acquiring this gate and publishing a writer must succeed before the reader returns.
        let guard = capture_gate.try_lock_for(Duration::from_secs(10));
        if guard.is_none() {
            cancellation.cancel();
        }
        let guard = guard.expect("capture guard must be released during the safe-snapshot wait");
        drop(guard);
        assert!(result.try_recv().is_err());
        let (later, _) = start(&persistence, false);
        writer.commit_transaction().unwrap();
        let context = result.recv_timeout(Duration::from_secs(10));
        if context.is_err() {
            cancellation.cancel();
        }
        let context = context.unwrap().unwrap();
        assert!(captures.try_recv().is_err());
        assert_eq!(reader.get(b"before_reader").unwrap(), None);
        read(&context, b"before_reader");
        reader.commit_transaction().unwrap();
        later.rollback_transaction().unwrap();
        worker.join().unwrap();
    });
}

#[test]
fn unsafe_candidate_is_replaced_before_any_application_read() {
    let persistence = Persistence::new();
    let (pivot, pivot_context) = start(&persistence, false);
    read(&pivot_context, b"first");
    let (first, _) = start(&persistence, false);
    write(&first, b"first");
    first.commit_transaction().unwrap();
    let reader = persistence.session(1 << 20);
    reader.begin_upgradeable_transaction().unwrap();
    reader.savepoint("before_snapshot").unwrap();
    let cancellation = reader.write_cancellation().unwrap();
    let (notify_capture, captures) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let context =
                reader.establish_serializable_snapshot_with(SAFE_READER, &mut |capture| {
                    capture()?;
                    notify_capture.send(()).unwrap();
                    Ok(())
                });
            finished.send(context).unwrap();
        });
        let capture = captures.recv_timeout(Duration::from_secs(10));
        if capture.is_err() {
            cancellation.cancel();
        }
        capture.unwrap();
        write(&pivot, b"pivot");
        pivot.commit_transaction().unwrap();
        let context = result.recv_timeout(Duration::from_secs(10));
        if context.is_err() {
            cancellation.cancel();
        }
        let context = context.unwrap().unwrap();
        captures
            .try_recv()
            .expect("unsafe snapshot must be recaptured");
        assert!(captures.try_recv().is_err());
        assert_eq!(reader.get(b"pivot").unwrap().unwrap(), b"changed");
        reader.rollback_to_savepoint("before_snapshot").unwrap();
        assert_eq!(reader.get(b"pivot").unwrap().unwrap(), b"changed");
        assert_eq!(
            reader.serializable_read_context().unwrap().unwrap().id(),
            context.id()
        );
        reader.commit_transaction().unwrap();
    });
}

#[test]
fn cancelled_wait_drops_candidate_without_installing_or_advancing_session_state() {
    let persistence = Persistence::new();
    let reader = persistence.session(1 << 20);
    reader.begin_upgradeable_transaction().unwrap();
    let (writer, _) = start(&persistence, false);
    write(&writer, b"later");
    writer.commit_transaction().unwrap();
    let (overlap, overlap_context) = start(&persistence, false);
    let abandoned = SerializableTransactionId::new(
        persistence.database_id(),
        [8; 16],
        overlap_context.id().allocation() + 1,
    )
    .unwrap();
    let cancellation = reader.write_cancellation().unwrap();
    let error = reader
        .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| {
            capture()?;
            cancellation.cancel();
            Ok(())
        })
        .err()
        .expect("waiting must observe cancellation");
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    cancellation.reset();
    assert!(reader.serializable_read_context().unwrap().is_none());
    assert!(matches!(
        persistence.actor_status(abandoned),
        Err(VersionError::UnknownTransaction)
    ));
    assert_eq!(reader.get(b"later").unwrap(), None);
    overlap.rollback_transaction().unwrap();
    let context = reader
        .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| capture())
        .unwrap();
    assert_eq!(
        context
            .safe_snapshot(&StorageReadControl::with_limit(1 << 20))
            .unwrap(),
        SafeSnapshot::Safe
    );
    assert_eq!(reader.get(b"later").unwrap().unwrap(), b"changed");
    reader.commit_transaction().unwrap();
}

#[test]
fn checkpoint_failure_cannot_publish_a_candidate_into_the_session() {
    for fault in [CheckpointFault::Reject, CheckpointFault::LoseReply] {
        for after in 1..=3 {
            let persistence = Persistence::new();
            let session = persistence.session(1 << 20);
            session.begin_upgradeable_transaction().unwrap();
            persistence.checkpoint_fault(after, fault);
            assert!(session
                .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| capture())
                .is_err());
            assert!(session.serializable_read_context().unwrap().is_none());
            let context = session
                .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| capture())
                .unwrap();
            read(&context, b"recovered");
            session.commit_transaction().unwrap();
            assert_eq!(persistence.state.lock().next, 0);
        }
    }
}

#[test]
fn rejected_or_replayed_capture_never_installs_a_participant() {
    for attempt in 0..4 {
        let persistence = Persistence::new();
        let session = persistence.session(1 << 20);
        session.begin_upgradeable_transaction().unwrap();
        let error =
            session.establish_serializable_snapshot_with(
                SAFE_READER,
                &mut |capture| match attempt {
                    0 => Ok(()),
                    1 => {
                        capture()?;
                        capture()
                    }
                    2 => {
                        capture()?;
                        Err(VersionError::InvalidEncoding("caller baseline failed"))
                    }
                    _ => Err(VersionError::InvalidEncoding("caller gate failed")),
                },
            );
        assert!(error.is_err());
        assert!(session.serializable_read_context().unwrap().is_none());
        let context = session
            .establish_serializable_snapshot_with(SAFE_READER, &mut |capture| capture())
            .unwrap();
        read(&context, b"after_recovery");
        session.commit_transaction().unwrap();
    }
}

#[test]
fn retained_session_cannot_admit_and_private_writes_cannot_move_the_initial_snapshot() {
    let persistence = Persistence::new();
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    session.put(b"private", b"original").unwrap();
    assert!(session
        .establish_serializable_snapshot_with(SAFE_READER, &mut |_| {
            panic!("private writes must reject admission before capture")
        })
        .is_err());
    let retained = session
        .open_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    assert!(retained
        .serializable_session()
        .unwrap()
        .establish_serializable_snapshot_with(SAFE_READER, &mut |_| {
            panic!("a retained reader cannot capture another participant")
        })
        .is_err());
    session.commit_transaction().unwrap();
    assert_eq!(session.get(b"private").unwrap().unwrap(), b"original");
}
