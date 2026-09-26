//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::{AtomicU64, Ordering};
use std::{path::Path, sync::Arc};

use super::{version, MAX_RECORD};
use crate::diskann_index::build::{
    DiskANNBuildCapture, DiskANNGenerationOptions, DiskANNMergeOptions, DiskANNPartitionOptions,
    DiskANNTemporaryBudget,
};
use crate::diskann_index::format::{
    DiskANNCanonicalOrigin, DiskANNGeneration, DiskANNManifest, DiskANNVectorVersion, PAGE_BYTES,
};
use crate::diskann_index::pages::{
    DiskANNOriginReader, DiskANNPageSource, DiskANNReadLimits, DiskANNReader, DiskANNRecordKey,
};
use crate::diskann_index::{
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, PQTrainingOptions,
};
use crate::key_value::conformance::{expect, expect_eq};
use crate::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

mod search;

const DIMENSIONS: u32 = 32;
const NODES: u64 = 1024;
const WORKSPACE: usize = 65_536;

fn build_parameters() -> DiskANNIndexParams {
    DiskANNIndexParams {
        max_degree: 4,
        build_list_size: 8,
        search_list_size: 16,
        beam_width: 2,
        pq_bytes: 4,
        ..DiskANNIndexParams::for_dimensions(DIMENSIONS).expect("fixed dimensions")
    }
}

fn raw(node: u64) -> [f32; DIMENSIONS as usize] {
    let mut raw = [0.0; DIMENSIONS as usize];
    raw[(node % 2) as usize] = if node % 4 < 2 { 1.0 } else { -1.0 };
    raw[2] = -0.0;
    raw
}

struct CanonicalInput(Arc<AtomicU64>);

impl DiskANNCanonicalRead for CanonicalInput {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()
    }
    fn dimensions(&self) -> u32 {
        DIMENSIONS
    }
    fn next_document_after(
        &self,
        after: Option<u64>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<u64>> {
        control.check()?;
        Ok((10..10 + NODES / 2)
            .chain(2000..2003)
            .chain([3000, u64::MAX])
            .find(|&doc| after.is_none_or(|last| doc > last)))
    }
    fn origin(
        &self,
        document: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        control.check()?;
        Ok(((10..10 + NODES / 2).contains(&document)
            || (2000..2003).contains(&document)
            || [3000, u64::MAX].contains(&document))
        .then(version))
    }
    fn visit_document(
        &self,
        document: u64,
        control: &StorageReadControl,
        visitor: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let origin = self.origin(document, control)?;
        if (10..10 + NODES / 2).contains(&document) {
            for ordinal in 0..2 {
                self.0.fetch_add(1, Ordering::Relaxed);
                visitor(
                    ordinal,
                    version(),
                    &raw((document - 10) * 2 + u64::from(ordinal)),
                )?;
            }
        } else if (2000..2003).contains(&document) {
            self.0.fetch_add(1, Ordering::Relaxed);
            visitor(0, version(), &[0.0; DIMENSIONS as usize])?;
        }
        Ok(origin)
    }
}

/// Build and seal a corpus whose raw vectors alone exceed the algorithm's workspace. Returns generation, peak controlled memory bytes and peak encrypted temporary bytes. The provider's opaque cache remains outside these buffers.
pub fn verify_diskann_built_generation(
    store: &Arc<dyn KeyValueStore>,
    directory: &Path,
) -> StorageBackendResult<(DiskANNGeneration, usize, u64)> {
    let control = StorageReadControl::with_limit(WORKSPACE);
    let temporary = DiskANNTemporaryBudget::new(16 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let mut stage = repository.allocate_stage(21, 22, &control)?;
    let generation = stage.generation();
    stage.start(&control)?;
    let capture = capture(generation, directory, &temporary, &control)?;
    let input = capture.input();
    let runs = input.build_partitions(
        directory,
        build_parameters(),
        DiskANNPartitionOptions {
            max_partition_points: 16,
            coarse_training: PQTrainingOptions {
                max_samples: 4,
                max_centroids: 3,
                max_iterations: 2,
                seed: 42,
            },
            max_depth: 0,
        },
    )?;
    let graph = input.merge_partitions(
        runs,
        directory,
        DiskANNMergeOptions {
            sort_buffer_records: 128,
        },
    )?;
    let manifest = capture.write_generation(&graph, generation_options(), &mut stage)?;
    expect(
        NODES * u64::from(DIMENSIONS) * 4 > WORKSPACE as u64,
        "raw corpus exceeds build workspace",
    )?;
    expect(
        repository.open_source(generation, &control).is_err(),
        "complete unsealed bytes remain unavailable",
    )?;
    let source = stage.seal(manifest, MAX_RECORD, &control)?;
    expect_eq(
        &stage.status(&control)?,
        &Some(DiskANNStageStatus::Sealed),
        "complete build seal",
    )?;
    expect_eq(
        &stage.seal(manifest, MAX_RECORD, &control)?.generation(),
        &generation,
        "versioned manifest supports idempotent seal",
    )?;
    expect(
        manifest.build_provenance().is_some(),
        "builder emits persisted provenance",
    )?;
    expect_eq(
        &manifest
            .build_provenance()
            .expect("checked provenance")
            .adjacency_digest(),
        &graph.summary().adjacency_digest,
        "sealed adjacency fingerprint",
    )?;
    drop(graph);
    let coverage = capture.finish(&manifest, &control)?;
    expect_eq(
        &manifest.origins(),
        &Some(coverage.origins()),
        "sealed complete origin summary",
    )?;
    drop((source, coverage, stage, repository));
    expect_eq(
        &temporary.used(),
        &0,
        "all encrypted temporary files released",
    )?;
    expect(
        temporary.peak() <= temporary.limit(),
        "shared encrypted file allowance",
    )?;
    expect(
        control.memory().peak() <= WORKSPACE,
        "complete build and seal use original workspace",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "complete builder releases reservations",
    )?;
    Ok((generation, control.memory().peak(), temporary.peak()))
}

/// Reopen the complete builder output after the provider, build input and temporary files have closed.
pub fn verify_diskann_built_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(WORKSPACE);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let source = repository.open_source(generation, &control)?;
    source.read_record(
        DiskANNRecordKey::Manifest,
        MAX_RECORD,
        &control,
        &mut |bytes| {
            let manifest = DiskANNManifest::decode(generation, bytes, &control)?;
            expect_eq(&manifest.input().nodes, &NODES, "reopened built node count")?;
            expect_eq(
                &manifest.input().side_vectors,
                &3,
                "reopened built side count",
            )?;
            let provenance = manifest
                .build_provenance()
                .ok_or_else(|| StorageBackendError::Other("missing build provenance".into()))?;
            expect_eq(
                &provenance.code_batch_nodes(),
                &31,
                "persisted code batch setting",
            )?;
            expect_eq(
                &provenance.entry_sample_points(),
                &256,
                "capped global entry sample",
            )
        },
    )?;
    verify_origins(source.clone(), &control)?;
    let reader = DiskANNReader::open(
        source.clone(),
        DIMENSIONS,
        build_parameters(),
        DiskANNReadLimits {
            resident_bytes: 16 << 10,
            cache_bytes: 12 << 10,
            max_in_flight_page_bytes: PAGE_BYTES * 2,
            max_record_bytes: MAX_RECORD,
        },
        &control,
    )?;
    for node in 0..NODES {
        let record = reader.read_node(node, &control)?;
        expect_eq(
            &(record.doc_id(), record.ordinal()),
            &(10 + node / 2, (node % 2) as u32),
            "reopened document and tensor ordinal",
        )?;
        expect_eq(&record.version(), &version(), "reopened canonical origin")?;
        expect(
            record
                .vector()
                .iter()
                .zip(raw(node))
                .all(|(actual, expected)| actual.to_bits() == expected.to_bits()),
            "reopened canonical coordinate bits",
        )?;
        expect(
            record.neighbors().contains(&((node + 1) % NODES)),
            "reopened global successor",
        )?;
        expect(
            record.neighbors().len() <= build_parameters().max_degree,
            "reopened degree limit",
        )?;
        expect(reader.code(node).is_some(), "reopened code stream")?;
    }
    let mut side = 0;
    reader.visit_side(&control, &mut |entry| {
        expect_eq(
            &(entry.doc_id(), entry.ordinal()),
            &(2000 + side, 0),
            "reopened exact-side identity",
        )?;
        expect_eq(&entry.version(), &version(), "reopened exact-side origin")?;
        side += 1;
        Ok(())
    })?;
    expect_eq(&side, &3, "complete reopened side stream")?;
    drop(repository);
    search::verify(source, &reader, &control)?;
    drop(reader);
    expect_eq(
        &control.memory().used(),
        &0,
        "reopened build releases reservations",
    )
}

fn capture(
    generation: DiskANNGeneration,
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNBuildCapture<CanonicalInput>> {
    let observed = Arc::new(AtomicU64::new(0));
    let capture = DiskANNBuildCapture::capture(
        generation,
        CanonicalInput(observed.clone()),
        directory,
        temporary,
        control,
    )?;
    expect_eq(
        &observed.load(Ordering::Relaxed),
        &(NODES + 3),
        "canonical ordinals captured exactly once",
    )?;
    Ok(capture)
}

fn verify_origins(
    source: Arc<dyn DiskANNPageSource>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let origins = DiskANNOriginReader::open(source, MAX_RECORD, control)?;
    for (document, count) in [
        (10, Some(2)),
        (73, Some(2)),
        (74, Some(2)),
        (521, Some(2)),
        (2000, Some(1)),
        (2002, Some(1)),
        (3000, Some(0)),
        (u64::MAX, Some(0)),
        (0, None),
        (522, None),
        (2999, None),
    ] {
        let origin = origins.origin(document, control)?;
        expect_eq(
            &origin.map(DiskANNCanonicalOrigin::count),
            &count,
            "reopened complete tensor cardinality",
        )?;
        expect_eq(
            &origin.map(DiskANNCanonicalOrigin::version),
            &count.map(|_| version()),
            "reopened exact original writer and mutation",
        )?;
    }
    drop(origins);
    Ok(())
}

fn generation_options() -> DiskANNGenerationOptions {
    DiskANNGenerationOptions {
        training: PQTrainingOptions {
            max_samples: 16,
            max_centroids: 4,
            max_iterations: 3,
            seed: 42,
        },
        code_batch_nodes: 31,
        side_batch_entries: 2,
        max_record_bytes: MAX_RECORD,
    }
}
