//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;

#[test]
fn weighted_ranges_reject_bad_identities_distances_order_and_truncation() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    for records in [
        vec![(0_u64, 1_u64, f64::NAN)],
        vec![(0, 1, -0.0)],
        vec![(0, 4, 1.0)],
        vec![(0, 0, 0.0)],
        vec![(1, 2, 1.0), (0, 2, 1.0)],
    ] {
        let mut writer = RunWriter::new(directory.path(), &temporary, &control).unwrap();
        for &(source, neighbor, distance) in &records {
            writer.append(&source.to_le_bytes()).unwrap();
            writer.append(&neighbor.to_le_bytes()).unwrap();
            writer.append(&distance.to_bits().to_le_bytes()).unwrap();
        }
        let run = writer.finish().unwrap();
        assert!(run
            .read(&control, |file| {
                let mut range = sort::Range::new(file, 0, records.len() as u64, 4, &control)?;
                while range.next()?.is_some() {}
                Ok(())
            })
            .is_err());
        drop(run);
        assert_eq!(temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
    let mut writer = RunWriter::new(directory.path(), &temporary, &control).unwrap();
    writer.append(&[0; 16]).unwrap();
    let run = writer.finish().unwrap();
    assert!(matches!(
        run.read(&control, |file| {
            sort::Range::new(file, 0, 1, 4, &control)?.next()
        }),
        Err(StorageBackendError::Backend { .. })
    ));
    drop(run);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn foreign_coverage_memory_cancellation_and_temporary_owners_are_rejected() {
    for kind in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let input = capture(directory.path(), &temporary, &control, 5);
        let partitions = input
            .build_partitions(directory.path(), parameters(32), partition_options())
            .unwrap();
        let other_control = match kind {
            1 => StorageReadControl::with_limit(64 << 10),
            2 => StorageReadControl::new(
                control.memory(),
                &crate::read_control::CancellationToken::new(),
            ),
            _ => control.clone(),
        };
        let other_temporary = if kind == 3 {
            DiskANNTemporaryBudget::new(1 << 20)
        } else {
            temporary.clone()
        };
        let other = capture(
            directory.path(),
            &other_temporary,
            &other_control,
            if kind == 0 { 6 } else { 5 },
        );
        assert!(other
            .merge_partitions(
                partitions,
                directory.path(),
                DiskANNMergeOptions {
                    sort_buffer_records: 2
                }
            )
            .is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 4);
        assert_eq!(input.read_node(0).unwrap().doc_id(), 10);
        drop((input, other));
        assert_eq!(temporary.used(), 0);
        assert_eq!(other_temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(other_control.memory().used(), 0);
    }
}

#[test]
fn shared_temporary_exhaustion_discards_candidates_and_preserves_capture() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let block = crate::temporary_file::BlockTemporaryFile::<4096>::physical_len_for(4096).unwrap();
    let temporary = DiskANNTemporaryBudget::new(7 * block);
    let input = capture(directory.path(), &temporary, &control, 128);
    let retained = temporary.used();
    let partitions = input
        .build_partitions(directory.path(), parameters(32), partition_options())
        .unwrap();
    let error = input
        .merge_partitions(
            partitions,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: 8,
            },
        )
        .err()
        .unwrap();
    let StorageBackendError::Backend { source, .. } = error else {
        panic!("expected shared temporary exhaustion")
    };
    assert!(source
        .downcast_ref::<super::super::super::DiskANNTemporaryError>()
        .is_some());
    assert_eq!(temporary.used(), retained);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    assert_eq!(input.read_node(127).unwrap().doc_id(), 137);
    drop(input);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
}

#[test]
fn merge_admission_and_zero_capacity_fail_without_a_partial_graph() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(8192);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 16);
    let retained = temporary.used();
    for capacity in [0, 1] {
        let partitions = input
            .build_partitions(directory.path(), parameters(32), partition_options())
            .unwrap();
        let error = input
            .merge_partitions(
                partitions,
                directory.path(),
                DiskANNMergeOptions {
                    sort_buffer_records: capacity,
                },
            )
            .err()
            .unwrap();
        if capacity == 1 {
            assert!(matches!(error, StorageBackendError::Memory(_)));
        }
        assert_eq!(temporary.used(), retained);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
    drop(input);
    assert_eq!(temporary.used(), 0);
}

#[test]
fn encrypted_candidate_corruption_is_an_error_without_exact_substitution() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 16);
    let retained = temporary.used();
    let partitions = input
        .build_partitions(directory.path(), parameters(32), partition_options())
        .unwrap();
    let (run, summary) = partitions.into_source(&input).unwrap();
    let mut bytes = std::fs::read(run.path()).unwrap();
    bytes[0] = 2;
    std::fs::write(run.path(), bytes).unwrap();
    assert!(matches!(
        merge(
            &input,
            run,
            &summary,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: 2
            }
        ),
        Err(StorageBackendError::Backend { .. })
    ));
    assert_eq!(temporary.used(), retained);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    assert_eq!(input.read_node(0).unwrap().doc_id(), 10);
    drop(input);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn output_replay_preserves_consumer_errors_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 16);
    let partitions = input
        .build_partitions(directory.path(), parameters(32), partition_options())
        .unwrap();
    let graph = input
        .merge_partitions(
            partitions,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: 2,
            },
        )
        .unwrap();
    assert!(matches!(
        graph.visit_neighbors(&mut |_, _| Err(uqa_core::memory::MemoryError::SizeOverflow.into())),
        Err(StorageBackendError::Memory(
            uqa_core::memory::MemoryError::SizeOverflow
        ))
    ));
    let mut visits = 0;
    assert!(matches!(
        graph.visit_neighbors(&mut |_, _| {
            visits += 1;
            control.cancellation().cancel();
            Ok(())
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(visits, 1);
    drop((graph, input));
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
