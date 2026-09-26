//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    publication::{setup, Resolver},
    *,
};
use uqa_storage::diskann_index::changes::{DiskANNPruneRequest, DiskANNPruneResult};
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, KeyValueDiskANNCanonical,
    KeyValueDiskANNPruner, RetainedDiskANNCanonical,
};
use uqa_storage::{RelationIdentity, StorageBackendResult};

fn published(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> (
    KeyValueDiskANNCanonical,
    RetainedDiskANNCanonical,
    KeyValueDiskANNPruner,
) {
    let canonical = setup(store);
    canonical.replace(1, &[vec![1.0, 0.0]], control).unwrap();
    let source = canonical
        .retain_for_index(&RelationIdentity::new("public", "publication_idx"), control)
        .unwrap();
    let parameters = source.index_parameters().unwrap();
    let scope = source.index_scope(&Resolver, control).unwrap();
    let repository = KeyValueDiskANNStore::connect(store, control).unwrap();
    repository.initialize(control).unwrap();
    let mut stage = repository.allocate_bound_stage(&scope, control).unwrap();
    let coverage =
        build_diskann_publication_fixture(source, &mut stage, parameters, control).unwrap();
    let sealed = repository.open_source(stage.generation(), control).unwrap();
    let pruner = KeyValueDiskANNPruner::open(sealed.clone(), 8192, control).unwrap();
    store.begin_transaction().unwrap();
    store
        .with_mutation(&mut |read, batch| {
            RetainedDiskANNCanonical::publish_generation(
                &coverage, &Resolver, &sealed, read, batch, control,
            )
        })
        .unwrap();
    assert!(
        evaluate(
            store,
            coverage.source(),
            &pruner,
            DiskANNPruneRequest {
                after: None,
                max_records: 64
            },
            control
        )
        .is_err(),
        "private head cannot authorize deletion"
    );
    store.commit_transaction().unwrap();
    let source = canonical
        .retain_for_index(&RelationIdentity::new("public", "publication_idx"), control)
        .unwrap();
    (canonical, source, pruner)
}

fn evaluate(
    store: &Arc<dyn KeyValueStore>,
    source: &RetainedDiskANNCanonical,
    pruner: &KeyValueDiskANNPruner,
    request: DiskANNPruneRequest,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNPruneResult> {
    let mut result = None;
    store.with_mutation(&mut |read, batch| {
        result = Some(source.prune_changes(&Resolver, pruner, (read, batch), request, control)?);
        Ok(())
    })?;
    Ok(result.expect("evaluated mutation"))
}

#[test]
fn diskann_pruning_caps_large_requests_and_resumes_without_loading_the_journal() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let (canonical, source, pruner) = published(&store, &control);
    let mut last = None;
    for _ in 0..70 {
        last = Some(canonical.replace(9, &[], &control).unwrap());
    }
    let query = StorageReadControl::with_limit(64 << 10);
    let first = evaluate(
        &store,
        &source,
        &pruner,
        DiskANNPruneRequest {
            after: None,
            max_records: usize::MAX,
        },
        &query,
    )
    .unwrap();
    assert_eq!((first.examined, first.removed), (64, 64));
    assert!(first.next.is_some());
    assert_eq!(query.memory().used(), 0);
    let second = evaluate(
        &store,
        &source,
        &pruner,
        DiskANNPruneRequest {
            after: first.next,
            max_records: usize::MAX,
        },
        &query,
    )
    .unwrap();
    assert_eq!((second.examined, second.removed, second.next), (7, 6, None));
    assert_eq!(
        canonical
            .retain(&control)
            .unwrap()
            .next_change_after(None, &control)
            .unwrap()
            .map(uqa_storage::diskann_index::format::DiskANNChangeIdentity::version),
        last
    );
    assert_eq!(query.memory().used(), 0);
    control.cancellation().cancel();
    assert!(
        evaluate(
            &store,
            &source,
            &pruner,
            DiskANNPruneRequest {
                after: None,
                max_records: 64
            },
            &query
        )
        .is_err(),
        "original cancellation survives a fresh invoking control"
    );
}

#[test]
fn diskann_pruning_resolves_original_receipts_without_replaying_or_deleting_late_writes() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let (canonical, source, pruner) = published(&store, &control);
        let peer = store.open_session().unwrap();
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        let mut calls = 0;
        assert!(store
            .with_mutation(&mut |read, batch| {
                calls += 1;
                let result = source.prune_changes(
                    &Resolver,
                    &pruner,
                    (read, batch),
                    DiskANNPruneRequest {
                        after: None,
                        max_records: 64,
                    },
                    &control,
                )?;
                assert_eq!((result.examined, result.removed), (1, 1));
                Ok(())
            })
            .is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        // Reject happens before the fixture records a physical attempt; lost replies expose the original prepared fingerprint.
        let original = persistence.state.lock().attempts.get(before).copied();
        assert_eq!(original.is_none(), fault == CommitFault::Reject);
        let late = KeyValueDiskANNCanonical::new(peer, "public.publication", "vector", 2)
            .unwrap()
            .replace(1, &[], &control)
            .unwrap();
        let retry_start = persistence.state.lock().attempts.len();
        store.commit_transaction().unwrap();
        assert_eq!(calls, 1);
        let attempts = persistence.state.lock();
        if let Some(original) = original {
            assert_eq!(
                attempts.attempts[before..]
                    .iter()
                    .filter(|&&fingerprint| fingerprint == original)
                    .count(),
                if fault == CommitFault::LoseBeforeCommit {
                    2
                } else {
                    1
                }
            );
        } else {
            assert_eq!(attempts.attempts.len(), retry_start + 1);
        }
        drop(attempts);
        let current = canonical.retain(&control).unwrap();
        assert_eq!(
            current
                .next_change_after(None, &control)
                .unwrap()
                .unwrap()
                .version(),
            late
        );
        let result = evaluate(
            &store,
            &source,
            &pruner,
            DiskANNPruneRequest {
                after: None,
                max_records: 64,
            },
            &control,
        )
        .unwrap();
        assert_eq!((result.examined, result.removed, result.next), (1, 0, None));
    }
}

#[test]
fn diskann_pruning_rejects_a_reconfigured_catalog_even_with_the_same_index_identity() {
    use uqa_storage::{CatalogFacade, KeyValueCatalog};
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let (canonical, source, pruner) = published(&store, &control);
    let mut parameters = source.index_parameters().unwrap();
    parameters.search_list_size += 1;
    let catalog = KeyValueCatalog::new(store.clone());
    let mut definition = catalog.load_catalog_indexes().unwrap().pop().unwrap();
    definition.parameters_json =
        serde_json::to_string(&parameters.to_catalog_map(2).unwrap()).unwrap();
    catalog.save_catalog_index_row(&definition).unwrap();
    let fresh = canonical
        .retain_for_index(&definition.relation, &control)
        .unwrap();
    let error = evaluate(
        &store,
        &fresh,
        &pruner,
        DiskANNPruneRequest {
            after: None,
            max_records: 64,
        },
        &control,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("current index configuration"),
        "{error}"
    );
    assert!(canonical
        .retain(&control)
        .unwrap()
        .next_change_after(None, &control)
        .unwrap()
        .is_some());
}
