//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual canonical providers retain generations and queries under shared original allowances.

use std::sync::Arc;

use super::{
    identity::{row, Resolver},
    runtime::diskann_runtime_fixture_options,
    setup_catalog_dimensions, FIELD, TABLE,
};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        format::{DiskANNGeneration, PAGE_BYTES},
        DiskANNIndexBinding, DiskANNIndexOptions, DiskANNQueryMetadata,
    },
    key_value::conformance::{expect, expect_eq},
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendResult, VectorIndex, VectorIndexOpenMode,
};

mod queries;

const DIMENSIONS: u32 = 128;
const DOCUMENTS: u64 = 512;
const REJECTED_PUBLICATION_BYTES: usize = 128 << 10;
const OWNER_BYTES: usize = 2 << 20;
const TEMPORARY_BYTES: u64 = 16 << 20;

fn options() -> StorageBackendResult<DiskANNIndexOptions> {
    let mut options = diskann_runtime_fixture_options(DIMENSIONS)?;
    options.parameters.max_degree = 4;
    options.parameters.build_list_size = 8;
    options.parameters.search_list_size = 16;
    options.parameters.beam_width = 2;
    options.parameters.pq_bytes = 4;
    // Include page payload and cache bookkeeping while keeping the graph larger than the cache.
    options.read.cache_bytes = 3 * PAGE_BYTES;
    options.partitions.max_partition_points = 16;
    options.partitions.coarse_training.max_samples = 4;
    options.generation.training.max_samples = 4;
    options.merge.sort_buffer_records = 128;
    options.generation.code_batch_nodes = 64;
    options.generation.side_batch_entries = 64;
    Ok(options)
}

fn raw(document: u64) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS as usize];
    vector[(document % 2) as usize] = if document % 4 < 2 { 1.0 } else { -1.0 };
    vector
}

fn open(
    backend: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    mode: VectorIndexOpenMode,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    backend.diskann_index(
        DiskANNIndexBinding {
            table: TABLE,
            field: FIELD,
            dimensions: DIMENSIONS,
            index: &row([91; 16])?.relation,
            resolver: Arc::new(Resolver),
            control,
        },
        options()?,
        temporary,
        mode,
    )
}

fn metadata(
    index: &dyn VectorIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNQueryMetadata> {
    index.diskann_query_metadata(control)?.ok_or_else(|| {
        crate::StorageBackendError::Other("resource fixture requires DiskANN metadata".into())
    })
}

/// Populate real provider vectors, reject publication under an insufficient original allowance, then construct under the same two-MiB allowance used for retained runtime queries. The caller must close all provider owners before invoking the paired reopen check.
pub fn verify_diskann_resource_source(
    backend: &dyn PersistentStorageBackend,
) -> StorageBackendResult<DiskANNGeneration> {
    let temporary = DiskANNTemporaryBudget::new(TEMPORARY_BYTES);
    {
        let setup = StorageReadControl::with_limit(8 << 20);
        let session = backend.open_controlled_session(&setup)?;
        setup_catalog_dimensions(&*session.catalog, DIMENSIONS)?;
        let mut definition = row([91; 16])?;
        definition.parameters_json =
            serde_json::to_string(&options()?.parameters.to_catalog_map(DIMENSIONS)?)?;
        session.catalog.save_catalog_index_row(&definition)?;
        session.backend.begin_transaction()?;
        let mut raw_index = open(
            &*session.backend,
            &temporary,
            &setup,
            VectorIndexOpenMode::Create,
        )?;
        for document in 1..=DOCUMENTS {
            raw_index.add(document, raw(document))?;
        }
        session.backend.commit_transaction()?;
    }
    rejected_publication(backend, &temporary)?;
    let build = StorageReadControl::with_limit(OWNER_BYTES);
    let generation = {
        let session = backend.open_controlled_session(&build)?;
        session.backend.begin_transaction()?;
        let mut index = open(
            &*session.backend,
            &temporary,
            &build,
            VectorIndexOpenMode::Restore,
        )?;
        index.initialize()?;
        session.backend.commit_transaction()?;
        metadata(&*index, &build)?.manifest.input().generation
    };
    expect_eq(
        &build.memory().used(),
        &0,
        "build owners release original memory",
    )?;
    expect(
        build.memory().peak() <= OWNER_BYTES,
        "actual-provider build and atomic publication share the original ceiling",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "build releases encrypted temporary bytes",
    )?;
    expect(
        temporary.peak() <= TEMPORARY_BYTES,
        "shared temporary build ceiling",
    )?;
    Ok(generation)
}

fn rejected_publication(
    backend: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(REJECTED_PUBLICATION_BYTES);
    {
        let session = backend.open_controlled_session(&control)?;
        let mut index = open(
            &*session.backend,
            temporary,
            &control,
            VectorIndexOpenMode::Restore,
        )?;
        let before = metadata(&*index, &control)?;
        session.backend.begin_transaction()?;
        let result = index.initialize();
        expect(
            matches!(result, Err(crate::StorageBackendError::Memory(_))),
            "undersized publication allowance rejects complete population reconciliation",
        )?;
        session.backend.rollback_transaction()?;
        expect_eq(
            &metadata(&*index, &control)?.manifest,
            &before.manifest,
            "rejected publication preserves selected generation",
        )?;
        expect_eq(
            &index.count()?,
            &(DOCUMENTS as usize),
            "rejected publication preserves all canonical vectors",
        )?;
    }
    expect_eq(
        &control.memory().used(),
        &0,
        "failed publication releases retained workspace",
    )?;
    expect(
        control.memory().peak() <= REJECTED_PUBLICATION_BYTES,
        "rejection never exceeds its original bound",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "failed publication releases temporary input",
    )
}

/// Cold reopen followed by coexisting original, changed and rebuilt views, concurrent actual queries, quota rejection and complete release. This checks accounted bytes, never timing or process RSS.
pub fn verify_diskann_resource_reopen(
    backend: &dyn PersistentStorageBackend,
    expected: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let owner = StorageReadControl::with_limit(OWNER_BYTES);
    let temporary = DiskANNTemporaryBudget::new(TEMPORARY_BYTES);
    {
        let session = backend.open_controlled_session(&owner)?;
        let mut live = open(
            &*session.backend,
            &temporary,
            &owner,
            VectorIndexOpenMode::Restore,
        )?;
        let before = metadata(&*live, &owner)?;
        expect_eq(
            &before.manifest.input().generation,
            &expected,
            "cold selected generation",
        )?;
        expect_eq(
            &before.manifest.input().nodes,
            &DOCUMENTS,
            "complete raw graph",
        )?;
        expect(
            before.manifest.layout().page_count() * PAGE_BYTES as u64
                > before.read_limits.cache_bytes as u64
                && DOCUMENTS * u64::from(DIMENSIONS) * 4 > before.read_limits.cache_bytes as u64,
            "actual graph and raw vectors both exceed page cache",
        )?;
        let original = live.snapshot()?;
        session.backend.begin_transaction()?;
        live.add(1, raw(4))?;
        live.delete(2)?;
        live.add(DOCUMENTS + 1, raw(4))?;
        session.backend.commit_transaction()?;
        let changed = live.snapshot()?;
        session.backend.begin_transaction()?;
        live.initialize()?;
        session.backend.commit_transaction()?;
        let rebuilt = live.snapshot()?;
        let after = metadata(&*rebuilt, &owner)?;
        expect(
            after.manifest.input().generation != expected,
            "replacement physical generation",
        )?;
        expect_eq(
            &metadata(&*original, &owner)?.manifest,
            &before.manifest,
            "original generation remains retained",
        )?;
        expect_eq(
            &metadata(&*changed, &owner)?.manifest,
            &before.manifest,
            "changed view retains original graph",
        )?;
        expect_eq(
            &metadata(&*changed, &owner)?
                .canonical_counts
                .unwrap()
                .changed_vectors(),
            &2,
            "changed view retains two scored replacement vectors",
        )?;
        queries::concurrent([&*original, &*changed, &*rebuilt], &owner)?;
        queries::rejection(&*original, &owner)?;
    }
    expect_eq(
        &owner.memory().used(),
        &0,
        "last provider and generation owners release accounted bytes",
    )?;
    expect(
        owner.memory().peak() <= OWNER_BYTES,
        "generations and concurrent queries share original ceiling",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "rebuild releases encrypted temporary bytes",
    )?;
    Ok(())
}
