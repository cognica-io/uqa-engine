//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence positions through the lock manager.

use super::*;

const KEY: SequencePositionKey = SequencePositionKey {
    object: [1; 16],
    definition: [2; 16],
};

const fn position(current: i64) -> SequencePosition {
    SequencePosition {
        logged: (current + 32, true),
        current,
        called: true,
        log_count: 32,
    }
}

fn positions_are_kept_and_dropped(manager: &RowLockManager) {
    let other = SequencePositionKey {
        object: [3; 16],
        ..KEY
    };
    {
        let mut guard = manager.lock_sequence_position(KEY).unwrap();
        assert!(guard.recorded().is_none() && guard.recordable() && !guard.crowded());
        guard.record(position(5)).unwrap();
        assert_eq!(guard.recorded().unwrap().position, position(5));
    }
    manager
        .lock_sequence_position(other)
        .unwrap()
        .record(position(9))
        .unwrap();
    let recorded = manager.sequence_position(KEY).unwrap().unwrap();
    assert_eq!((recorded.position, recorded.fresh), (position(5), true));
    let all = manager.sequence_positions().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[&other].position, position(9));
    {
        let mut guard = manager.lock_sequence_position(KEY).unwrap();
        guard.retain(&|key| *key == KEY).unwrap();
        assert_eq!(guard.recorded().unwrap().position, position(5));
    }
    assert_eq!(manager.sequence_positions().unwrap().len(), 1);
    let mut guard = manager.lock_sequence_position(KEY).unwrap();
    guard.discard().unwrap();
    assert!(guard.recorded().is_none());
    drop(guard);
    assert!(manager.sequence_position(KEY).unwrap().is_none());
    assert!(manager.sequence_positions().unwrap().is_empty());
}

#[test]
fn a_manager_without_a_database_file_keeps_positions_in_the_process() {
    positions_are_kept_and_dropped(&RowLockManager::new());
}

#[test]
fn a_manager_of_a_database_file_keeps_positions_in_its_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("positions.db");
    positions_are_kept_and_dropped(&RowLockManager::for_database_file(&path));
    let manager = RowLockManager::for_database_file(&path);
    manager
        .lock_sequence_position(KEY)
        .unwrap()
        .record(position(7))
        .unwrap();
    drop(manager);
    // Another manager of the file reads what the first one left, as a position of an earlier run.
    let recorded = RowLockManager::for_database_file(&path)
        .sequence_position(KEY)
        .unwrap()
        .unwrap();
    assert_eq!((recorded.position, recorded.fresh), (position(7), false));
}

#[test]
fn sessions_wait_for_the_position_one_of_them_holds() {
    let manager = RowLockManager::new();
    let advanced = std::sync::atomic::AtomicU64::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..500 {
                    let mut guard = manager.lock_sequence_position(KEY).unwrap();
                    let current = guard.recorded().map_or(0, |held| held.position.current);
                    guard.record(position(current + 1)).unwrap();
                    advanced.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(
        manager
            .sequence_position(KEY)
            .unwrap()
            .unwrap()
            .position
            .current,
        2000
    );
}

#[test]
fn a_position_of_an_earlier_run_continues_only_the_record_it_names() {
    let kept = RecordedSequencePosition {
        position: position(5),
        fresh: false,
    };
    assert_eq!(kept.continuing((37, true)), Some(position(5)));
    assert_eq!(kept.continuing((36, true)), None);
    assert_eq!(kept.continuing((37, false)), None);
    let fresh = RecordedSequencePosition {
        fresh: true,
        ..kept
    };
    assert_eq!(fresh.continuing((1, false)), Some(position(5)));
}
