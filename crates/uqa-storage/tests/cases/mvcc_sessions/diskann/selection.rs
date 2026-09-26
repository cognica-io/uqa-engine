//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::publication::{setup, Resolver};
use super::*;
use uqa_storage::diskann_index::pages::{DiskANNOriginReader, DiskANNPageSource};
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, RetainedDiskANNCanonical,
};
use uqa_storage::RelationIdentity;

#[test]
fn diskann_query_source_survives_original_publication_receipt_resolution() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let canonical = setup(&store);
        let version = canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let relation = RelationIdentity::new("public", "publication_idx");
        let source = canonical.retain_for_index(&relation, &control).unwrap();
        let parameters = source.index_parameters().unwrap();
        let scope = source.index_scope(&Resolver, &control).unwrap();
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        let coverage =
            build_diskann_publication_fixture(source, &mut stage, parameters, &control).unwrap();
        let sealed = repository
            .open_source(stage.generation(), &control)
            .unwrap();
        store.begin_transaction().unwrap();
        let mut calls = 0;
        store
            .with_mutation(&mut |read, batch| {
                calls += 1;
                RetainedDiskANNCanonical::publish_generation(
                    &coverage, &Resolver, &sealed, read, batch, &control,
                )
            })
            .unwrap();
        let held = canonical.retain_for_index(&relation, &control).unwrap();
        let generation = stage.generation();
        drop((coverage, stage, sealed, repository));
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(store.commit_transaction().is_err());
        let selected = held.selected_source(&Resolver, &control).unwrap().unwrap();
        assert_eq!(selected.generation(), generation);
        assert_eq!(
            DiskANNOriginReader::open(selected, 8192, &control)
                .unwrap()
                .origin(1, &control)
                .unwrap()
                .unwrap()
                .version(),
            version
        );
        let original = persistence.state.lock().attempts.get(before).copied();
        persistence.state.lock().commit_fault = CommitFault::None;
        let peer = store.open_session().unwrap();
        peer.put(b"independent-after-publication", b"preserved")
            .unwrap();
        store.commit_transaction().unwrap();
        assert_eq!(calls, 1);
        let receipts = persistence.state.lock();
        if let Some(original) = original {
            assert_eq!(receipts.attempts[before], original);
            if fault == CommitFault::LoseBeforeCommit {
                assert_eq!(*receipts.attempts.last().unwrap(), original);
            }
        } else {
            assert!(fault == CommitFault::Reject);
        }
        drop(receipts);
        let committed = canonical
            .retain_for_index(&relation, &control)
            .unwrap()
            .selected_source(&Resolver, &control)
            .unwrap()
            .unwrap();
        assert_eq!(committed.generation(), generation);
        assert_eq!(
            held.selected_source(&Resolver, &control)
                .unwrap()
                .unwrap()
                .generation(),
            generation
        );
        assert_eq!(
            store
                .get(b"independent-after-publication")
                .unwrap()
                .as_deref(),
            Some(&b"preserved"[..])
        );
    }
}
