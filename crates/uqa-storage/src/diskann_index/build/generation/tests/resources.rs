//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;

struct FailingSink {
    generation: DiskANNGeneration,
    writes: usize,
}
impl DiskANNBuildSink for FailingSink {
    fn generation(&self) -> DiskANNGeneration {
        self.generation
    }
    fn write_record(
        &mut self,
        _: DiskANNRecordKey,
        _: &[u8],
        _: usize,
        _: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.writes += 1;
        Err(uqa_core::memory::MemoryError::SizeOverflow.into())
    }
    fn write_graph_page(
        &mut self,
        _: u64,
        _: &[u8],
        _: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        unreachable!("record failure must stop build")
    }
}

#[test]
fn invalid_options_and_foreign_owners_write_nothing_and_sink_failures_keep_their_type() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 2, 4, 3);
    let graph = merged(&input, directory.path());
    let retained = temporary.used();
    let mut sink = FailingSink {
        generation: generation(),
        writes: 0,
    };
    for field in 0..4 {
        let mut options = options();
        match field {
            0 => options.code_batch_nodes = 0,
            1 => options.side_batch_entries = 0,
            2 => options.training.seed += 1,
            _ => {
                options.max_record_bytes =
                    DiskANNManifest::ENCODED_BYTES + DiskANNBuildProvenance::ENCODED_BYTES - 1;
            }
        }
        assert!(input.write_generation(&graph, options, &mut sink).is_err());
        assert_eq!(sink.writes, 0);
    }
    sink.generation = DiskANNGeneration::new([1; 16], 2, 3, 5).unwrap();
    assert!(input
        .write_generation(&graph, options(), &mut sink)
        .is_err());
    assert_eq!(sink.writes, 0);
    sink.generation = generation();
    let other_control = StorageReadControl::with_limit(64 << 10);
    let other = capture(directory.path(), &temporary, &other_control, 2, 4, 3);
    assert!(other
        .write_generation(&graph, options(), &mut sink)
        .is_err());
    assert_eq!(sink.writes, 0);
    drop(other);
    assert!(matches!(
        input.write_generation(&graph, options(), &mut sink),
        Err(StorageBackendError::Memory(
            uqa_core::memory::MemoryError::SizeOverflow
        ))
    ));
    assert_eq!(sink.writes, 1);
    assert_eq!(temporary.used(), retained);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        input.write_generation(&graph, options(), &mut sink),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(sink.writes, 1);
    drop((graph, input));
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generation_memory_and_artifact_allowances_fail_without_leaking_capture_or_output() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 2, 4, 3);
    let graph = merged(&input, directory.path());
    let retained = temporary.used();
    let reservation = control
        .memory()
        .reserve(control.memory().limit() - 16)
        .unwrap();
    let mut sink = FailingSink {
        generation: generation(),
        writes: 0,
    };
    assert!(matches!(
        input.write_generation(&graph, options(), &mut sink),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(sink.writes, 0);
    drop(reservation);
    let physical = MemoryBudget::new(1024);
    let mut sink = DiskANNMemoryBuilder::new(generation(), &physical);
    assert!(matches!(
        input.write_generation(&graph, options(), &mut sink),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(
        physical.used() > 0,
        "partially written artifacts remain caller-owned"
    );
    assert_eq!(temporary.used(), retained);
    assert_eq!(control.memory().used(), 0);
    drop((sink, graph, input));
    assert_eq!(physical.used(), 0);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
