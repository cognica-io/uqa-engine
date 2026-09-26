//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity::{row, Resolver},
    publication::{build, publish},
    setup, FIELD, TABLE,
};
use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNGeneration, DiskANNVectorVersion, PAGE_BYTES},
    pages::{DiskANNOriginReader, DiskANNPageSource, DiskANNReadLimits},
    DiskANNCanonicalScorer, RetainedDiskANNIndex,
};
use crate::key_value::{
    conformance::{expect, expect_eq},
    KeyValueDiskANNCanonical, KeyValueDiskANNStore, RetainedDiskANNCanonical,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult, VectorIndex,
};
use std::sync::Arc;

fn capture(
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedDiskANNCanonical> {
    canonical.retain_for_index(&row([91; 16])?.relation, control)
}

fn owned(
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedDiskANNIndex<RetainedDiskANNCanonical>> {
    Ok(capture(canonical, control)?
        .into_vector_index(
            &Resolver,
            DiskANNReadLimits {
                resident_bytes: 65_536,
                cache_bytes: PAGE_BYTES,
                max_in_flight_page_bytes: 2 * PAGE_BYTES,
                max_record_bytes: 8192,
            },
            control,
        )?
        .expect("published retained index"))
}

fn check_owned(
    index: &dyn VectorIndex,
    canonical: &RetainedDiskANNCanonical,
    count: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let fresh = StorageReadControl::with_limit(1);
    let nested = index
        .snapshot_with_control(&fresh)?
        .snapshot_with_control(&fresh)?;
    expect_eq(
        &nested.index_kind(),
        &"diskann",
        "nested snapshot retains the physical index",
    )?;
    expect_eq(&nested.count()?, &count, "retained tensor ordinal count")?;
    expect(
        nested.contains_document(1)?,
        "retained nonempty tensor membership",
    )?;
    expect(
        !nested.contains_document(2)?,
        "empty tensor has no vector membership",
    )?;
    expect(
        !nested.contains_document(99)?,
        "absent document has no vector membership",
    )?;
    let exact =
        DiskANNCanonicalScorer::new(canonical, &[1.0, 0.0], control)?.search_exact_knn(10)?;
    expect_eq(
        &nested.search_knn(&[1.0, 0.0], 10)?,
        &exact,
        "owned snapshot keeps raw canonical scores",
    )?;
    let threshold =
        DiskANNCanonicalScorer::new(canonical, &[1.0, 0.0], control)?.search_threshold(0.0)?;
    expect_eq(
        &nested.search_threshold(&[1.0, 0.0], 0.0)?,
        &threshold,
        "owned snapshot keeps exact thresholds",
    )?;
    expect_eq(
        &fresh.memory().used(),
        &0,
        "nested snapshot does not replace the original allowance",
    )
}

fn check(
    view: &RetainedDiskANNCanonical,
    generation: DiskANNGeneration,
    version: DiskANNVectorVersion,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let source = view
        .selected_source(&Resolver, control)?
        .expect("published fixture");
    expect_eq(
        &source.generation(),
        &generation,
        "selected head stays on its query view",
    )?;
    let origins = DiskANNOriginReader::open(source.clone(), 8192, control)?;
    expect_eq(
        &origins
            .origin(1, control)?
            .map(DiskANNCanonicalOrigin::version),
        &Some(version),
        "physical origins stay with the selected head",
    )?;
    expect_eq(
        &view.origin(1, control)?,
        &Some(version),
        "canonical input stays on its query view",
    )?;
    let mut count = 0;
    source.read_graph_pages(&[0], control, &mut |id, bytes| {
        expect_eq(&id, &0, "selected graph page")?;
        expect_eq(&bytes.len(), &PAGE_BYTES, "selected graph page width")?;
        count += 1;
        Ok(())
    })?;
    expect_eq(&count, &1, "one complete selected graph page")?;
    let query = view
        .query(
            &Resolver,
            DiskANNReadLimits {
                resident_bytes: 65_536,
                cache_bytes: 0,
                max_in_flight_page_bytes: 2 * PAGE_BYTES,
                max_record_bytes: 8192,
            },
            control,
        )?
        .expect("published query");
    let actual = query.search_knn(&[1.0, 0.0], 10, control)?.postings;
    let exact = DiskANNCanonicalScorer::new(view, &[1.0, 0.0], control)?.search_exact_knn(10)?;
    let bits = |postings: &uqa_core::PostingList| {
        postings
            .iter()
            .map(|posting| (posting.doc_id, posting.payload.score.to_bits()))
            .collect::<Vec<_>>()
    };
    expect_eq(
        &bits(&actual),
        &bits(&exact),
        "selected generation merges the retained canonical changes and full tensors",
    )
}

/// Exercise real private and committed query generation selection on a disposable provider. Physical construction occurs after the transaction's data snapshot; retained private readers survive refresh, undo and release of every build owner.
pub fn verify_diskann_query_views(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    KeyValueCatalog::new(store.clone()).save_catalog_index_row(&row([91; 16])?)?;
    canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    canonical.replace(2, &[], &control)?;
    let missing = capture(&canonical, &control)?;
    expect(
        missing.selected_source(&Resolver, &control)?.is_none(),
        "missing head remains absent",
    )?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let (private, private_generation, private_version) =
        private_view(store, &canonical, &repository, &control)?;
    check(&private, private_generation, private_version, &control)?;
    expect(
        missing.selected_source(&Resolver, &control)?.is_none(),
        "old absent selection never follows a later publication",
    )?;
    let (first, stage) = build(&canonical, &repository, &control)?;
    publish(store, &first, &control)?;
    let first_generation = stage.generation();
    let first_version = canonical.retain(&control)?.origin(1, &control)?.unwrap();
    let held = capture(&canonical, &control)?;
    let held_index = owned(&canonical, &control)?;
    let second_version = canonical.replace(1, &[vec![0.0, 1.0]], &control)?;
    let (second, second_stage) = build(&canonical, &repository, &control)?;
    publish(store, &second, &control)?;
    let second_generation = second_stage.generation();
    drop((first, stage, second, second_stage, repository));
    check(&held, first_generation, first_version, &control)?;
    check_owned(&held_index, &held, 2, &control)?;
    check(&private, private_generation, private_version, &control)?;
    check(
        &capture(&canonical, &control)?,
        second_generation,
        second_version,
        &control,
    )?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        held.selected_source(&Resolver, &tiny).is_err(),
        "query source respects invoking allowance",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed query selection releases its allowance",
    )?;
    cancellation(&canonical, &control)?;
    configuration(store, &canonical, &control)?;
    Ok(second_generation)
}

fn private_view(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<(
    RetainedDiskANNCanonical,
    DiskANNGeneration,
    DiskANNVectorVersion,
)> {
    store.begin_transaction()?;
    store.savepoint("before_generation")?;
    let version = canonical.replace(1, &[vec![0.0, 1.0], vec![1.0, 0.0]], control)?;
    repository.initialize(control)?;
    let (coverage, stage) = build(canonical, repository, control)?;
    publish(store, &coverage, control)?;
    let held = capture(canonical, control)?;
    let held_index = owned(canonical, control)?;
    check(&held, stage.generation(), version, control)?;
    private_replacement(store, canonical, repository, &held, control)?;
    store.savepoint("published_generation")?;
    let peer = store.open_session()?;
    let peer_canonical = KeyValueDiskANNCanonical::new(peer.clone(), TABLE, FIELD, 2)?;
    peer_canonical.replace(3, &[vec![0.5, 0.5]], control)?;
    expect(
        canonical.retain(control)?.origin(3, control)?.is_none(),
        "physical selection does not advance SQL data",
    )?;
    expect(
        capture(&peer_canonical, control)?
            .selected_source(&Resolver, control)?
            .is_none(),
        "private head is invisible to peer",
    )?;
    store.refresh_transaction_snapshot(control.cancellation())?;
    check(
        &capture(canonical, control)?,
        stage.generation(),
        version,
        control,
    )?;
    expect(
        canonical.retain(control)?.origin(3, control)?.is_some(),
        "explicit command refresh advances canonical data",
    )?;
    store.rollback_to_savepoint("published_generation")?;
    check(
        &capture(canonical, control)?,
        stage.generation(),
        version,
        control,
    )?;
    expect(
        canonical.retain(control)?.origin(3, control)?.is_none(),
        "savepoint restores its committed boundary",
    )?;
    store.rollback_to_savepoint("before_generation")?;
    expect(
        capture(canonical, control)?
            .selected_source(&Resolver, control)?
            .is_none(),
        "undo clears the selecting private head",
    )?;
    store.rollback_transaction()?;
    let generation = stage.generation();
    drop((coverage, stage, peer_canonical, peer));
    check_owned(&held_index, &held, 2, control)?;
    Ok((held, generation, version))
}

fn private_replacement(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    held: &RetainedDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.savepoint("original_head")?;
    let scope = held.index_scope(&Resolver, control)?;
    let mut selection_key = b"\0uqa-diskann-v1\0\x05".to_vec();
    for id in [
        scope.table_object(),
        scope.storage_generation(),
        scope.index_object(),
    ] {
        selection_key.extend_from_slice(&id);
    }
    let bytes = store.get(&selection_key)?.unwrap();
    store.put(&selection_key, &bytes)?;
    expect(
        capture(canonical, control)?
            .selected_source(&Resolver, control)
            .is_err(),
        "equal private head bytes do not recreate a discarded physical source",
    )?;
    expect(
        held.selected_source(&Resolver, control)?.is_some(),
        "older private query keeps its source after head replacement",
    )?;
    store.rollback_to_savepoint("original_head")?;
    let (next, stage) = build(canonical, repository, control)?;
    publish(store, &next, control)?;
    check(
        &capture(canonical, control)?,
        stage.generation(),
        held.origin(1, control)?.unwrap(),
        control,
    )?;
    expect(
        held.selected_source(&Resolver, control)?.is_some(),
        "superseded private query keeps its original publication",
    )?;
    store.rollback_to_savepoint("original_head")?;
    store.release_savepoint("original_head")?;
    Ok(())
}

fn configuration(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let original = row([91; 16])?;
    let held = capture(canonical, control)?;
    let mut parameters = held.index_parameters().unwrap();
    parameters.search_list_size += 1;
    let mut changed = original.clone();
    changed.parameters_json = serde_json::to_string(&parameters.to_catalog_map(2)?)?;
    catalog.save_catalog_index_row(&changed)?;
    expect(
        capture(canonical, control)?
            .selected_source(&Resolver, control)
            .is_err(),
        "same identity with different configuration cannot select old pages",
    )?;
    expect(
        held.selected_source(&Resolver, control)?.is_some(),
        "retained catalog keeps its original configuration",
    )?;
    catalog.save_catalog_index_row(&original)
}

fn cancellation(
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let original = StorageReadControl::with_limit(1 << 20);
    let view = capture(canonical, &original)?;
    let source = view.selected_source(&Resolver, control)?.unwrap();
    original.cancellation().cancel();
    expect(
        view.selected_source(&Resolver, control).is_err(),
        "query view retains original cancellation",
    )?;
    expect(
        source
            .read_graph_pages(&[0], control, &mut |_, _| Ok(()))
            .is_err(),
        "selected pages retain canonical query cancellation",
    )
}

/// Reopen selection from persisted head/configuration without a process-local publication resource.
pub fn verify_diskann_query_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    // Scope resolution retains both decoded catalog records; their existing 16x decode reservations share this allowance with page/origin reads.
    let control = StorageReadControl::with_limit(65_536);
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
    let held = capture(&canonical, &control)?;
    let version = held.origin(1, &control)?.unwrap();
    check(&held, generation, version, &control)?;
    check_owned(&owned(&canonical, &control)?, &held, 2, &control)
}
