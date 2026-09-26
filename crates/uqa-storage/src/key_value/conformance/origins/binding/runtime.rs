//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity::{row, Resolver},
    setup, FIELD, TABLE,
};
use crate::{
    diskann_index::{
        build::{
            DiskANNGenerationOptions, DiskANNMergeOptions, DiskANNPartitionOptions,
            DiskANNTemporaryBudget,
        },
        format::{DiskANNGeneration, PAGE_BYTES},
        pages::{DiskANNPageSource, DiskANNReadLimits},
        DiskANNIndexOptions, PQTrainingOptions, PersistentDiskANNIndex,
    },
    key_value::{
        conformance::{expect, expect_eq},
        KeyValueDiskANNCanonical, KeyValueVectorIndex,
    },
    read_control::StorageReadControl,
    vector_index::DiskANNIndexParams,
    CatalogFacade, KeyValueCatalog, KeyValueStore, StorageBackendResult, VectorIndex,
};
use std::sync::Arc;

/// Small deterministic provider acceptance settings, never runtime defaults.
pub fn diskann_runtime_fixture_options(
    dimensions: u32,
) -> StorageBackendResult<DiskANNIndexOptions> {
    let training = PQTrainingOptions {
        max_samples: 8,
        max_centroids: 2,
        max_iterations: 2,
        seed: 42,
    };
    Ok(DiskANNIndexOptions {
        parameters: DiskANNIndexParams::for_dimensions(dimensions)?,
        read: DiskANNReadLimits {
            resident_bytes: 65_536,
            cache_bytes: PAGE_BYTES,
            max_in_flight_page_bytes: 2 * PAGE_BYTES,
            max_record_bytes: 8192,
        },
        partitions: DiskANNPartitionOptions {
            max_partition_points: 8,
            coarse_training: PQTrainingOptions {
                max_centroids: 3,
                ..training
            },
            max_depth: 0,
        },
        merge: DiskANNMergeOptions {
            sort_buffer_records: 8,
        },
        generation: DiskANNGenerationOptions {
            training,
            code_batch_nodes: 4,
            side_batch_entries: 4,
            max_record_bytes: 8192,
        },
    })
}

fn canonical(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<KeyValueDiskANNCanonical> {
    KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)
}

fn runtime(
    store: &Arc<dyn KeyValueStore>,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<impl VectorIndex> {
    let options = diskann_runtime_fixture_options(2)?;
    PersistentDiskANNIndex::new(
        canonical(store)?.bind(
            row([91; 16])?.relation,
            Arc::new(Resolver),
            options.read,
            control,
        )?,
        options,
        temporary,
    )
}

fn generation(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    canonical(store)?
        .retain_for_index(&row([91; 16])?.relation, control)?
        .selected_source(&Resolver, control)?
        .map(|source| source.generation())
        .ok_or_else(|| {
            crate::StorageBackendError::Other(
                "runtime fixture has no selected DiskANN generation".into(),
            )
        })
}

fn scores(index: &dyn VectorIndex, expected: &[(u64, f64)]) -> StorageBackendResult<()> {
    let postings = index.search_knn(&[1.0, 0.0], 10)?;
    expect_eq(
        &postings
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .collect::<Vec<_>>(),
        &expected
            .iter()
            .map(|&(id, score)| (id, score.to_bits()))
            .collect::<Vec<_>>(),
        "runtime literal complete tensor scores",
    )
}

fn create(
    store: &Arc<dyn KeyValueStore>,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let options = diskann_runtime_fixture_options(2)?;
    let definition = row([91; 16])?;
    catalog.save_catalog_index_row(&definition)?;
    let mut raw = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    raw.add(1, vec![1.0, -0.0])?;
    raw.add_many(2, vec![vec![0.0, 1.0], vec![1.0, 0.0]])?;
    raw.add(3, vec![-1.0, 0.0])?;
    expect(
        runtime(store, temporary, control).is_err(),
        "restore never creates a missing head",
    )?;
    expect(
        canonical(store)?
            .create_index(&definition.relation, &Resolver, options, temporary, control)
            .is_err(),
        "creation requires the caller transaction",
    )?;
    store.begin_transaction()?;
    store.put(b"runtime-outer-write", b"kept")?;
    catalog.save_catalog_index_row(&definition)?;
    let mut bad = options;
    bad.generation.max_record_bytes = 1;
    expect(
        canonical(store)?
            .create_index(&definition.relation, &Resolver, bad, temporary, control)
            .is_err(),
        "failed construction rolls back adoption",
    )?;
    let mut unreadable = options;
    unreadable.read.resident_bytes = 1;
    expect(
        canonical(store)?
            .create_index(
                &definition.relation,
                &Resolver,
                unreadable,
                temporary,
                control,
            )
            .is_err(),
        "creation rejects a sealed generation that cannot be admitted for queries",
    )?;
    expect_eq(
        &raw.count()?,
        &4,
        "failed creation preserves original raw ordinals",
    )?;
    expect(
        canonical(store)?
            .retain(control)?
            .origin(1, control)
            .is_err(),
        "failed creation leaves the original unstamped raw field",
    )?;
    expect_eq(
        &store.get(b"runtime-outer-write")?,
        &Some(b"kept".to_vec()),
        "structural undo keeps earlier caller effects",
    )?;
    canonical(store)?.create_index(&definition.relation, &Resolver, options, temporary, control)?;
    expect(
        store.in_transaction(),
        "construction never commits the outer transaction",
    )?;
    Ok(())
}

/// Exercise real first construction, bounded legacy adoption, rebuild and empty-generation publication through the public `VectorIndex` interface, including private undo and failure atomicity.
pub fn verify_diskann_runtime_lifecycle(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 21);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let options = diskann_runtime_fixture_options(2)?;
    let definition = row([91; 16])?;
    create(store, &temporary, &control)?;
    let mut index = runtime(store, &temporary, &control)?;
    scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)])?;
    expect_eq(&index.count()?, &4, "adoption preserves complete tensors")?;
    let first = generation(store, &control)?;
    let held = index.snapshot()?;
    store.savepoint("runtime-user")?;
    index.add(1, vec![-1.0, 0.0])?;
    index.delete(2)?;
    index.add(4, vec![0.0, 1.0])?;
    expect_eq(
        &generation(store, &control)?,
        &first,
        "ordinary writes preserve the base",
    )?;
    index.initialize()?;
    expect(
        generation(store, &control)? != first,
        "initialize publishes a replacement base",
    )?;
    let rebuilt = index.snapshot()?;
    scores(&*rebuilt, &[(1, -1.0), (3, -1.0), (4, 0.0)])?;
    index.clear()?;
    let empty = index.snapshot()?;
    expect_eq(
        &empty.count()?,
        &0,
        "clear publishes empty canonical membership",
    )?;
    let empty_source = canonical(store)?
        .retain_for_index(&definition.relation, &control)?
        .into_vector_index(&Resolver, options.read, &control)?
        .ok_or_else(|| crate::StorageBackendError::Other("missing empty runtime head".into()))?;
    expect_eq(
        &empty_source.manifest().input().nodes,
        &0,
        "clear publishes an actually empty graph",
    )?;
    store.rollback_to_savepoint("runtime-user")?;
    scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)])?;
    scores(&*rebuilt, &[(1, -1.0), (3, -1.0), (4, 0.0)])?;
    expect_eq(&empty.count()?, &0, "undone empty snapshot remains valid")?;
    expect_eq(
        &generation(store, &control)?,
        &first,
        "savepoint restores the original head",
    )?;
    store.commit_transaction()?;
    expect(
        index.clear().is_err(),
        "clear requires a caller transaction",
    )?;
    expect(
        index.initialize().is_err(),
        "rebuild requires a caller transaction",
    )?;
    store.begin_transaction()?;
    let mut bad = options;
    bad.generation.max_record_bytes = 1;
    let mut constrained = PersistentDiskANNIndex::new(
        canonical(store)?.bind(
            definition.relation.clone(),
            Arc::new(Resolver),
            bad.read,
            &control,
        )?,
        bad,
        &temporary,
    )?;
    expect(
        constrained.clear().is_err(),
        "failed empty build restores its own clear",
    )?;
    scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)])?;
    expect_eq(
        &generation(store, &control)?,
        &first,
        "failed clear preserves the original head",
    )?;
    index.clear()?;
    index.add(5, vec![1.0, 0.0])?;
    index.initialize()?;
    store.commit_transaction()?;
    scores(&index, &[(5, 1.0)])?;
    scores(&*held, &[(1, 1.0), (2, 1.0), (3, -1.0)])?;
    let final_generation = generation(store, &control)?;
    expect_eq(
        &temporary.used(),
        &0,
        "finished builds release encrypted temporary input",
    )?;
    Ok(final_generation)
}

/// Reopen the actual provider after all original owners have been dropped.
pub fn verify_diskann_runtime_reopen(
    store: &Arc<dyn KeyValueStore>,
    expected: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let mut index = runtime(store, &temporary, &control)?;
    expect_eq(
        &generation(store, &control)?,
        &expected,
        "cold restore keeps its actual generation",
    )?;
    scores(&index, &[(5, 1.0)])?;
    index.add(6, vec![0.0, 1.0])?;
    scores(&index, &[(5, 1.0), (6, 0.0)])?;
    expect_eq(
        &generation(store, &control)?,
        &expected,
        "cold writes do not rebuild",
    )
}

/// A constructor adopts one complete raw namespace. A racing legacy insertion must conflict even when its key did not exist in the captured source.
pub fn verify_diskann_runtime_adoption_conflicts(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let definition = row([91; 16])?;
    catalog.save_catalog_index_row(&definition)?;
    let peer = store.open_session()?;
    let options = diskann_runtime_fixture_options(2)?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(1 << 21);
    let mut raw = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    let mut late = KeyValueVectorIndex::new(peer.clone(), TABLE, FIELD, 2);
    for creator_first in [false, true] {
        raw.clear()?;
        raw.add(1, vec![1.0, 0.0])?;
        peer.begin_transaction()?;
        late.add(99, vec![0.0, 1.0])?;
        store.begin_transaction()?;
        canonical(store)?.create_index(
            &definition.relation,
            &Resolver,
            options,
            &temporary,
            &control,
        )?;
        if creator_first {
            store.commit_transaction()?;
            expect(
                peer.commit_transaction().is_err(),
                "older unstamped insertion conflicts with completed adoption",
            )?;
            peer.rollback_transaction()?;
            scores(&runtime(store, &temporary, &control)?, &[(1, 1.0)])?;
        } else {
            peer.commit_transaction()?;
            expect(
                store.commit_transaction().is_err(),
                "adoption conflicts with a newly committed raw ordinal",
            )?;
            store.rollback_transaction()?;
            expect_eq(
                &raw.count()?,
                &2,
                "failed adoption preserves the winning raw insertion",
            )?;
            expect(
                canonical(store)?
                    .retain_for_index(&definition.relation, &control)?
                    .selected_source(&Resolver, &control)?
                    .is_none(),
                "failed adoption publishes no head",
            )?;
        }
    }
    Ok(())
}
