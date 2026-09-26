//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::catalog::{resolve_scope, DiskANNIndexResolver, DiskANNIndexScope};
use uqa_storage::StorageBackendResult;

struct Resolver;
impl DiskANNIndexResolver for Resolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        _: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        assert_eq!(definition, "fixture definition");
        assert_eq!(table, [7; 16]);
        Ok([9; 16])
    }
}

pub(super) fn scope(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> DiskANNIndexScope {
    store
        .put(b"catalog-fixture", b"fixture definition")
        .unwrap();
    let mut scope = None;
    store
        .with_read_view(&mut |read| {
            let revision = read
                .record_revision(b"catalog-fixture")?
                .expect("actual catalog record");
            scope = Some(resolve_scope(
                &Resolver,
                ([7; 16], [8; 16]),
                Some("fixture definition"),
                &revision,
                read.control(),
                control,
                control,
            )?);
            Ok(())
        })
        .unwrap();
    scope.unwrap()
}

#[test]
fn diskann_catalog_identity_resolves_lost_mapping_replies_without_replaying_or_committing_the_caller(
) {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let scope = scope(&store, &control);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        store.begin_transaction().unwrap();
        store.put(b"caller-private", b"kept").unwrap();
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(repository.allocate_bound_stage(&scope, &control).is_err());
        assert!(repository.allocate_bound_stage(&scope, &control).is_err());
        let original = store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap();
        uqa_storage::key_value::KeyValueDiskANNMaintenance::run(&store, &control).unwrap();
        assert_eq!(store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap(), original);
        persistence.state.lock().commit_fault = CommitFault::None;
        repository.commit_pending().unwrap();
        {
            let state = persistence.state.lock();
            assert_eq!(state.attempts[before], *state.attempts.last().unwrap());
            assert_eq!(
                state.attempts.len() - before,
                if fault == CommitFault::LoseBeforeCommit {
                    2
                } else {
                    1
                }
            );
        }
        let attempts = persistence.state.lock().attempts.len();
        let stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        assert_eq!(persistence.state.lock().attempts.len(), attempts + 1);
        assert!(store.in_transaction());
        assert_eq!(
            store.get(b"caller-private").unwrap().as_deref(),
            Some(&b"kept"[..])
        );
        let other = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let next = other
            .allocate_bound_stage(&scope, &control)
            .unwrap()
            .generation();
        assert_eq!(
            (next.table(), next.index()),
            (stage.generation().table(), stage.generation().index())
        );
        assert!(next.generation() > stage.generation().generation());
        store.rollback_transaction().unwrap();
    }
}

#[test]
fn diskann_catalog_identity_competing_creator_keeps_one_durable_mapping() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let scope = scope(&store, &control);
    let first = KeyValueDiskANNStore::connect(&store, &control).unwrap();
    first.initialize(&control).unwrap();
    persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
    assert!(first.allocate_bound_stage(&scope, &control).is_err());
    let second = KeyValueDiskANNStore::connect(&store, &control).unwrap();
    let selected = second
        .allocate_bound_stage(&scope, &control)
        .unwrap()
        .generation();
    assert!(first.commit_pending().is_err());
    first.rollback_pending().unwrap();
    let adopted = first
        .allocate_bound_stage(&scope, &control)
        .unwrap()
        .generation();
    assert_eq!(
        (adopted.table(), adopted.index()),
        (selected.table(), selected.index())
    );
    assert!(adopted.generation() > selected.generation());
}
