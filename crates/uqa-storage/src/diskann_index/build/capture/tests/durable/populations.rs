//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    format::{DiskANNChangeIdentity, DiskANNVectorVersion},
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNPopulationState,
    DiskANNPopulationWitness, DiskANNQueryRead,
};
use crate::{StorageBackendError, StorageBackendResult};

struct Current {
    rows: Vec<(u64, DiskANNCanonicalOrigin)>,
    control: StorageReadControl,
    dimensions: u32,
    repeat_cursor: bool,
    missing_origin: bool,
}

impl Current {
    fn new(rows: Vec<(u64, DiskANNCanonicalOrigin)>) -> Self {
        Self {
            rows,
            control: StorageReadControl::with_limit(64 << 10),
            dimensions: 2,
            repeat_cursor: false,
            missing_origin: false,
        }
    }
}

impl DiskANNCanonicalRead for Current {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        after: Option<u64>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<u64>> {
        self.check_control(control)?;
        Ok(self
            .rows
            .iter()
            .find(|(document, _)| self.repeat_cursor || after.is_none_or(|after| *document > after))
            .map(|(document, _)| *document))
    }
    fn origin(
        &self,
        _: u64,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("census must use complete origin shapes");
    }
    fn visit_document(
        &self,
        _: u64,
        _: &StorageReadControl,
        _: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("census must not decode coordinates");
    }
}

impl DiskANNQueryRead for Current {
    fn document_origin(
        &self,
        document: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check_control(control)?;
        Ok((!self.missing_origin)
            .then(|| {
                self.rows
                    .iter()
                    .find(|(found, _)| *found == document)
                    .map(|(_, origin)| *origin)
            })
            .flatten())
    }
    fn next_change_after(
        &self,
        _: Option<u64>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        panic!("census must not infer coverage from the journal");
    }
}

fn origin(revision: u64, count: u64) -> DiskANNCanonicalOrigin {
    DiskANNCanonicalOrigin::new(version(revision), 2, count).unwrap()
}

fn built_origins() -> DiskANNOriginReader {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let physical = MemoryBudget::new(1 << 20);
    let captured = capture(directory.path(), &temporary, &control, 4, true);
    let (manifest, sink) = build(&captured, directory.path(), &physical);
    let source = Arc::new(sink.finish(manifest, &control).unwrap());
    DiskANNOriginReader::open(source, options().max_record_bytes, &control).unwrap()
}

#[test]
fn diskann_population_census_uses_complete_origins_including_empty_tensors() {
    let built = built_origins();
    let current = Current::new(vec![
        (0, origin(1, 1)),
        (1, origin(20, 2)),
        (2, origin(30, 0)),
        (3, origin(4, 0)),
        (4, origin(40, 3)),
    ]);
    let control = StorageReadControl::with_limit(8192);
    let mut witnesses = Vec::new();
    let state = DiskANNPopulationState::capture(&current, &built, &control, &mut |witness| {
        witnesses.push(witness);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        (
            state.counts().current_vectors(),
            state.counts().changed_vectors()
        ),
        (6, 5)
    );
    assert_eq!(state.generation(), built.manifest().input().generation);
    assert_eq!(state.dimensions(), 2);
    assert_eq!(
        witnesses
            .iter()
            .map(|witness| (
                witness.document(),
                witness.covered(),
                witness.origin().count()
            ))
            .collect::<Vec<_>>(),
        [
            (0, true, 1),
            (1, false, 2),
            (2, false, 0),
            (3, true, 0),
            (4, false, 3)
        ]
    );
    for witness in witnesses {
        assert_eq!(
            DiskANNPopulationWitness::decode(
                &witness.encode(),
                state.generation(),
                witness.document(),
                witness.origin()
            )
            .unwrap(),
            witness
        );
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_population_census_does_not_treat_an_older_origin_as_build_coverage() {
    let built = built_origins();
    let current = Current::new(vec![(0, origin(1, 1)), (2, origin(1, 1))]);
    let control = StorageReadControl::with_limit(8192);
    let mut coverage = Vec::new();
    let state = DiskANNPopulationState::capture(&current, &built, &control, &mut |witness| {
        coverage.push((witness.document(), witness.covered()));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        (
            state.counts().current_vectors(),
            state.counts().changed_vectors()
        ),
        (2, 1)
    );
    assert_eq!(coverage, [(0, true), (2, false)]);
    let empty = Current::new(Vec::new());
    let state =
        DiskANNPopulationState::capture(&empty, &built, &control, &mut |_| panic!("empty corpus"))
            .unwrap();
    assert_eq!(
        (
            state.counts().current_vectors(),
            state.counts().changed_vectors()
        ),
        (0, 0)
    );
}

#[test]
fn diskann_population_census_propagates_invalid_sources_callbacks_and_controls() {
    let built = built_origins();
    let control = StorageReadControl::with_limit(8192);
    for fault in 0..5 {
        let mut current = Current::new(vec![(0, origin(1, 1))]);
        match fault {
            0 => current.dimensions = 3,
            1 => current.repeat_cursor = true,
            2 => current.missing_origin = true,
            3 => current.rows[0].1 = origin(1, 2),
            4 => current.rows[0].1 = DiskANNCanonicalOrigin::new(version(1), 3, 1).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            DiskANNPopulationState::capture(&current, &built, &control, &mut |_| Ok(())).is_err(),
            "fault {fault}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let current = Current::new(vec![(0, origin(1, 1))]);
    let mut calls = 0;
    let error = DiskANNPopulationState::capture(&current, &built, &control, &mut |_| {
        calls += 1;
        Err(StorageBackendError::Other("witness batch rejected".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("witness batch rejected"));
    assert_eq!(calls, 1);
    let tiny = StorageReadControl::with_limit(0);
    assert!(
        DiskANNPopulationState::capture(&current, &built, &tiny, &mut |_| panic!("failed read"))
            .is_err()
    );
    assert_eq!(tiny.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(8192);
    cancelled.cancellation().cancel();
    assert!(
        DiskANNPopulationState::capture(&current, &built, &cancelled, &mut |_| panic!(
            "cancelled caller"
        ))
        .is_err()
    );
    current.control.cancellation().cancel();
    assert!(
        DiskANNPopulationState::capture(&current, &built, &control, &mut |_| panic!(
            "cancelled source"
        ))
        .is_err()
    );
    assert_eq!(control.memory().used(), 0);
}
