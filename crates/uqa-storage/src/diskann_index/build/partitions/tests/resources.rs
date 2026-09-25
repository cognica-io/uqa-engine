//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;

#[test]
fn partition_admission_failures_preserve_capture_and_release_every_new_owner() {
    let mut failures = 0;
    let mut successes = 0;
    for limit in [2048, 4096, 8192, 16384] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(limit);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let input = capture(directory.path(), &temporary, &control, 16, true);
        let before = temporary.used();
        match input.build_partitions(directory.path(), parameters(32), options(16, 0)) {
            Ok(runs) => {
                assert!(!edges(&runs).is_empty());
                successes += 1;
            }
            Err(StorageBackendError::Memory(_)) => failures += 1,
            Err(error) => panic!("unexpected build failure: {error}"),
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(temporary.used(), before);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        assert_eq!(input.read_node(0).unwrap().doc_id(), 10);
        drop(input);
        assert_eq!(temporary.used(), 0);
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn shared_disk_failure_invalid_options_and_cancelled_work_leave_no_edge_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let limit =
        crate::temporary_file::BlockTemporaryFile::<4096>::physical_len_for(16 * (44 + 32 * 4))
            .unwrap();
    let temporary = DiskANNTemporaryBudget::new(limit);
    let input = capture(directory.path(), &temporary, &control, 16, true);
    let error = input
        .build_partitions(directory.path(), parameters(32), options(4, 0))
        .err()
        .unwrap();
    let StorageBackendError::Backend { source, .. } = error else {
        panic!("expected shared temporary limit")
    };
    assert!(source
        .downcast_ref::<super::super::super::DiskANNTemporaryError>()
        .is_some());
    assert_eq!(temporary.used(), limit);
    assert_eq!(control.memory().used(), 0);
    let mut invalid = options(1, 0);
    assert!(input
        .build_partitions(directory.path(), parameters(32), invalid)
        .is_err());
    invalid = options(4, 33);
    assert!(input
        .build_partitions(directory.path(), parameters(32), invalid)
        .is_err());
    invalid = options(4, 4);
    invalid.coarse_training.max_centroids = 2;
    assert!(input
        .build_partitions(directory.path(), parameters(32), invalid)
        .is_err());
    control.cancellation().cancel();
    assert!(matches!(
        input.build_partitions(directory.path(), parameters(32), options(4, 4)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    drop(input);
    assert_eq!(temporary.used(), 0);
}

#[test]
fn cancellation_between_capacity_windows_stops_before_the_next_leaf() {
    let control = StorageReadControl::with_limit(4096);
    let mut visits = 0;
    let result = leaf::windows(Ids::All(100), 4, &control, &mut |_| {
        visits += 1;
        control.cancellation().cancel();
        Ok(())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(visits, 1);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn candidate_run_errors_preserve_typed_consumer_failures_and_reject_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 8, true);
    let runs = input
        .build_partitions(directory.path(), parameters(32), options(4, 0))
        .unwrap();
    let failure =
        runs.visit_edges(&mut |_, _| Err(uqa_core::memory::MemoryError::SizeOverflow.into()));
    assert!(matches!(
        failure,
        Err(StorageBackendError::Memory(
            uqa_core::memory::MemoryError::SizeOverflow
        ))
    ));
    let mut ciphertext = std::fs::read(runs.edges.path()).unwrap();
    ciphertext[0] = 2;
    std::fs::write(runs.edges.path(), ciphertext).unwrap();
    assert!(matches!(
        runs.visit_edges(&mut |_, _| Ok(())),
        Err(StorageBackendError::Backend { .. })
    ));
    drop((input, runs));
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
