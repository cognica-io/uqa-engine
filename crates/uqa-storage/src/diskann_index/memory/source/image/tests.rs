//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invalid canonical successors fail before exposing any physical payload.

use super::*;
use crate::{
    diskann_index::{build::DiskANNTemporaryBudget, memory::image},
    read_control::StorageReadControl,
};

fn corrupt(source: &Canonical, fault: usize, original: &StorageReadControl) -> Canonical {
    let mut canonical = source.clone();
    match fault {
        0 => canonical.covered_generation = None,
        1 => canonical.counts = DiskANNCanonicalCounts::default(),
        2 => canonical.revision = None,
        3 => {
            canonical.changes = canonical.changes.with_remove(&8).unwrap();
            canonical.counts = DiskANNCanonicalCounts::new(
                canonical.counts.current_vectors(),
                canonical.counts.changed_vectors()
                    - canonical.documents.get(&8).unwrap().origin.count(),
            )
            .unwrap();
        }
        4 => canonical
            .changes
            .try_insert(u64::MAX, canonical.revision.unwrap())
            .unwrap(),
        5 => canonical.documents = canonical.documents.with_remove(&5).unwrap(),
        6 => {
            let origin = canonical.documents.get(&1).unwrap().origin;
            canonical
                .documents
                .try_insert(
                    1,
                    Tensor {
                        origin,
                        values: Budgeted::new(Vec::new(), original.memory().empty_reservation()),
                    },
                )
                .unwrap();
        }
        7 => canonical
            .changes
            .try_insert(8, canonical.documents.get(&1).unwrap().origin.version())
            .unwrap(),
        8 => {
            let origin = DiskANNCanonicalOrigin::new(canonical.revision.unwrap(), 2, 0).unwrap();
            canonical
                .documents
                .try_insert(
                    0,
                    Tensor {
                        origin,
                        values: Budgeted::new(Vec::new(), original.memory().empty_reservation()),
                    },
                )
                .unwrap();
        }
        _ => unreachable!(),
    }
    canonical
}

#[test]
fn memory_diskann_image_preparation_rejects_corrupt_origins_counts_and_changes_before_output() {
    let original = StorageReadControl::with_limit(1 << 20);
    let mut wire = &include_bytes!("../../tests/images/fixtures/evaluated.uqa")[13..];
    let source = image::read(
        &mut ImageInput {
            reader: &mut wire,
            control: &original,
        },
        2,
        &DiskANNTemporaryBudget::new(1 << 20),
    )
    .unwrap();
    assert!(wire.is_empty());
    for fault in 0..9 {
        let canonical = corrupt(source.index.canonical(), fault, &original);
        let index = source.index.with_canonical(canonical).unwrap();
        let invoking = StorageReadControl::with_limit(128 * 1024);
        let mut reader = image::reader(
            &index,
            &source.physical,
            source.options,
            &original,
            &invoking,
        )
        .unwrap();
        let mut failure = None;
        for _ in 0..100 {
            match reader.prepare(1) {
                Ok(progress) => {
                    assert!(!progress.ready, "invalid source {fault}");
                    assert_eq!(progress.positions, 1);
                }
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        let failure = failure.expect("bounded fixture must reach its invalid metadata");
        assert_eq!(invoking.memory().used(), 0);
        assert!(reader.read(&mut [0; 1]).is_err());
        let mut bytes = Vec::new();
        let synchronous = image::write(
            &index,
            &source.physical,
            source.options,
            &original,
            &mut bytes,
            &invoking,
        )
        .unwrap_err();
        assert_eq!(synchronous.to_string(), failure);
        assert!(bytes.is_empty());
        assert_eq!(invoking.memory().used(), 0);
    }
    drop(source);
    assert_eq!(original.memory().used(), 0);
}
