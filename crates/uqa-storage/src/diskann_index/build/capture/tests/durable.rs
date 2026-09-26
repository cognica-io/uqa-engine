//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryBudget;

use super::{version, Source};
use crate::diskann_index::{
    build::{
        DiskANNBuildCapture, DiskANNGenerationOptions, DiskANNMergeOptions,
        DiskANNPartitionOptions, DiskANNTemporaryBudget,
    },
    format::{
        DiskANNCanonicalOrigin, DiskANNGeneration, DiskANNManifest, DiskANNOriginEntry,
        DiskANNOriginLayout, DiskANNOriginSummary, ORIGIN_BATCH_DOCUMENTS,
    },
    pages::{
        DiskANNMemoryBuilder, DiskANNMemorySource, DiskANNOriginReader, DiskANNPageSource,
        DiskANNRecordKey,
    },
    PQTrainingOptions,
};
use crate::{read_control::StorageReadControl, vector_index::DiskANNIndexParams};

mod failures;

fn options() -> DiskANNGenerationOptions {
    DiskANNGenerationOptions {
        training: PQTrainingOptions {
            max_samples: 4,
            max_centroids: 2,
            max_iterations: 2,
            seed: 42,
        },
        code_batch_nodes: 2,
        side_batch_entries: 2,
        max_record_bytes: 32 << 10,
    }
}

fn build(
    capture: &DiskANNBuildCapture<Source>,
    directory: &std::path::Path,
    physical: &MemoryBudget,
) -> (DiskANNManifest, DiskANNMemoryBuilder) {
    let input = capture.input();
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 4,
        beam_width: 2,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).unwrap()
    };
    let runs = input
        .build_partitions(
            directory,
            parameters,
            DiskANNPartitionOptions {
                max_partition_points: 4,
                coarse_training: PQTrainingOptions {
                    max_centroids: 3,
                    ..options().training
                },
                max_depth: 0,
            },
        )
        .unwrap();
    let graph = input
        .merge_partitions(
            runs,
            directory,
            DiskANNMergeOptions {
                sort_buffer_records: 4,
            },
        )
        .unwrap();
    let mut sink = DiskANNMemoryBuilder::new(input.coverage().generation(), physical);
    let manifest = capture
        .write_generation(&graph, options(), &mut sink)
        .unwrap();
    (manifest, sink)
}

fn capture(
    directory: &std::path::Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    documents: u64,
    emit_vectors: bool,
) -> DiskANNBuildCapture<Source> {
    DiskANNBuildCapture::capture(
        DiskANNGeneration::new([11; 16], 1, 2, 3).unwrap(),
        Source {
            revision: 1,
            documents,
            emit_vectors,
            control: control.clone(),
        },
        directory,
        temporary,
        control,
    )
    .unwrap()
}

#[test]
fn complete_origins_outlive_canonical_capture_and_fit_one_bounded_lookup_batch() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let physical = MemoryBudget::new(1 << 20);
    let captured = capture(directory.path(), &temporary, &control, 130, true);
    let (manifest, sink) = build(&captured, directory.path(), &physical);
    let source = Arc::new(sink.finish(manifest, &control).unwrap());
    let coverage = captured.finish(&manifest, &control).unwrap();
    assert_eq!(manifest.origins(), Some(coverage.origins()));
    assert_eq!(coverage.origins().documents(), 130);
    drop(coverage);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
    let query = StorageReadControl::with_limit(8192);
    let reader = DiskANNOriginReader::open(source.clone(), 8192, &query).unwrap();
    for document in [0, 1, 2, 63, 64, 65, 127, 128, 129] {
        let origin = reader.origin(document, &query).unwrap().unwrap();
        assert_eq!(origin.version(), version(document + 1));
        assert_eq!(origin.count(), u64::from(document == 0 || document == 2));
    }
    for document in [130, u64::MAX] {
        assert_eq!(reader.origin(document, &query).unwrap(), None);
    }
    let tiny = StorageReadControl::with_limit(DiskANNOriginLayout::MAX_ENCODED_BYTES - 1);
    assert!(reader.origin(0, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    query.cancellation().cancel();
    assert!(reader.origin(130, &query).is_err());
    drop((reader, source));
    assert_eq!(query.memory().used(), 0);
    assert_eq!(physical.used(), 0);
}

#[test]
fn empty_tensors_have_distinct_durable_evidence_from_an_absent_corpus() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let physical = MemoryBudget::new(1 << 20);
    let absent = capture(directory.path(), &temporary, &control, 0, false);
    let present = capture(directory.path(), &temporary, &control, 3, false);
    let (absent_manifest, absent_sink) = build(&absent, directory.path(), &physical);
    let (present_manifest, present_sink) = build(&present, directory.path(), &physical);
    assert_eq!(
        absent_manifest.input().coverage,
        present_manifest.input().coverage
    );
    assert_ne!(absent_manifest.origins(), present_manifest.origins());
    assert!(present.finish(&absent_manifest, &control).is_err());
    for (manifest, sink, expected) in [
        (absent_manifest, absent_sink, None),
        (present_manifest, present_sink, Some(0)),
    ] {
        let source = Arc::new(sink.finish(manifest, &control).unwrap());
        let reader = DiskANNOriginReader::open(source, 8192, &control).unwrap();
        assert_eq!(
            reader
                .origin(0, &control)
                .unwrap()
                .map(DiskANNCanonicalOrigin::count),
            expected
        );
    }
    drop(absent);
    assert_eq!(temporary.used(), 0);
    assert_eq!(physical.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

fn record(
    source: &DiskANNMemorySource,
    key: DiskANNRecordKey,
    control: &StorageReadControl,
) -> Vec<u8> {
    let mut result = Vec::new();
    source
        .read_record(key, 32 << 10, control, &mut |bytes| {
            result.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    result
}

#[test]
fn origin_sealing_rejects_missing_repeated_corrupt_and_wrong_cardinality_streams() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let physical = MemoryBudget::new(1 << 20);
    let captured = capture(directory.path(), &temporary, &control, 130, false);
    let (manifest, sink) = build(&captured, directory.path(), &physical);
    let source = sink.finish(manifest, &control).unwrap();
    let layout = DiskANNOriginLayout::new(manifest.input().generation, 2, 130).unwrap();
    for fault in 0..5 {
        let mut candidate = manifest;
        let mut sink = DiskANNMemoryBuilder::new(manifest.input().generation, &physical);
        let mut hash = Sha256::new();
        for first in [0, 64, 128] {
            let key = DiskANNRecordKey::Origins(first);
            let mut bytes = record(&source, key, &control);
            if fault == 0 && first == 64 {
                continue;
            }
            if fault == 1 && first == 64 {
                bytes = record(&source, DiskANNRecordKey::Origins(0), &control);
            }
            if fault == 2 && first == 0 {
                *bytes.last_mut().unwrap() ^= 1;
            }
            if fault == 3 {
                let batch = layout.decode(first, &bytes, &control).unwrap();
                let mut entries: Vec<_> = (0..batch.len())
                    .map(|index| batch.entry(index).unwrap())
                    .collect();
                if first == 0 {
                    entries[0] = DiskANNOriginEntry::new(
                        0,
                        crate::diskann_index::format::DiskANNCanonicalOrigin::new(version(1), 2, 1)
                            .unwrap(),
                    );
                }
                bytes = layout.encode(first, &entries, &control).unwrap().to_vec();
                hash.update(layout.decode(first, &bytes, &control).unwrap().bytes());
            }
            sink.write_record(key, &bytes, &control).unwrap();
        }
        if fault == 3 {
            candidate = candidate
                .with_origins(DiskANNOriginSummary::new(130, hash.finalize().into()).unwrap())
                .unwrap();
        }
        if fault == 4 {
            sink.write_record(
                DiskANNRecordKey::Origins(192),
                &record(&source, DiskANNRecordKey::Origins(128), &control),
                &control,
            )
            .unwrap();
        }
        assert!(sink.finish(candidate, &control).is_err(), "fault {fault}");
    }
    drop((source, captured));
    assert_eq!(temporary.used(), 0);
    assert_eq!(physical.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_origin_artifact_can_exceed_capture_and_lookup_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(16 << 10);
    let physical = MemoryBudget::new(1 << 20);
    let captured = capture(directory.path(), &temporary, &control, 1025, false);
    let (manifest, sink) = build(&captured, directory.path(), &physical);
    let source = Arc::new(sink.finish(manifest, &control).unwrap());
    drop(captured);
    assert_eq!(temporary.used(), 0);
    assert!(physical.used() > 16 << 10);
    let reader = DiskANNOriginReader::open(source, 8192, &control).unwrap();
    for document in [0, ORIGIN_BATCH_DOCUMENTS as u64 - 1, 64, 1024] {
        assert_eq!(
            reader
                .origin(document, &control)
                .unwrap()
                .unwrap()
                .version(),
            version(document + 1)
        );
    }
    drop(reader);
    assert_eq!(physical.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
