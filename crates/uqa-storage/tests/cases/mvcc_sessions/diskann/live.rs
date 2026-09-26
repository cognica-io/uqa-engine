//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    publication::{setup, Resolver},
    *,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_storage::diskann_index::{
    catalog::DiskANNIndexResolver,
    format::{DiskANNChangeIdentity, PAGE_BYTES},
    pages::DiskANNReadLimits,
};
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, RetainedDiskANNCanonical,
};
use uqa_storage::{RelationIdentity, StorageBackendResult, VectorIndex};

struct CountedResolver(AtomicUsize);

impl DiskANNIndexResolver for CountedResolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Resolver.resolve(definition, table, control)
    }
}

#[test]
fn diskann_live_writes_resolve_original_receipts_without_replaying_catalog_or_tensor() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let canonical = setup(&store);
        canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let relation = RelationIdentity::new("public", "publication_idx");
        let source = canonical.retain_for_index(&relation, &control).unwrap();
        let scope = source.index_scope(&Resolver, &control).unwrap();
        let parameters = source.index_parameters().unwrap();
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        let coverage =
            build_diskann_publication_fixture(source, &mut stage, parameters, &control).unwrap();
        let sealed = repository
            .open_source(stage.generation(), &control)
            .unwrap();
        store
            .with_mutation(&mut |read, batch| {
                RetainedDiskANNCanonical::publish_generation(
                    &coverage, &Resolver, &sealed, read, batch, &control,
                )
            })
            .unwrap();
        let resolver = Arc::new(CountedResolver(AtomicUsize::new(0)));
        let live = canonical
            .bind(
                relation,
                resolver.clone(),
                DiskANNReadLimits {
                    resident_bytes: 65_536,
                    cache_bytes: 0,
                    max_in_flight_page_bytes: 2 * PAGE_BYTES,
                    max_record_bytes: 8192,
                },
                &control,
            )
            .unwrap();
        let calls = resolver.0.load(Ordering::Relaxed);
        let attempts = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(live.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]]).is_err());
        assert_eq!(resolver.0.load(Ordering::Relaxed), calls + 1);
        persistence.state.lock().commit_fault = CommitFault::None;
        store.commit_transaction().unwrap();
        assert_eq!(resolver.0.load(Ordering::Relaxed), calls + 1);
        let receipts = persistence.state.lock();
        assert_eq!(
            receipts.attempts[attempts],
            *receipts.attempts.last().unwrap()
        );
        assert_eq!(
            receipts.attempts.len() - attempts,
            if fault == CommitFault::LoseBeforeCommit {
                2
            } else {
                1
            }
        );
        drop(receipts);
        let snapshot = live.snapshot().unwrap();
        assert_eq!(snapshot.count().unwrap(), 3);
        assert_eq!(
            snapshot
                .search_knn(&[1.0, 0.0], 10)
                .unwrap()
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score))
                .collect::<Vec<_>>(),
            vec![(1, 1.0), (2, 1.0)]
        );
        let canonical = uqa_storage::key_value::KeyValueDiskANNCanonical::new(
            store.clone(),
            "public.publication",
            "vector",
            2,
        )
        .unwrap();
        let source = canonical.retain(&control).unwrap();
        let version = source.origin(2, &control).unwrap().unwrap();
        assert_eq!(
            source.next_change_after(Some(1), &control).unwrap(),
            Some(DiskANNChangeIdentity::new(2, version))
        );
        assert!(source
            .next_change_after(Some(2), &control)
            .unwrap()
            .is_none());
    }
}
