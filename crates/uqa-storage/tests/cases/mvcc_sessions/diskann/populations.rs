//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise common population effects through the actual versioned batch/session owner. Persistent provider format opt-in and lifecycle wiring have their own acceptance boundary.

use super::publication::{setup, Resolver};
use super::*;
use uqa_storage::diskann_index::{
    build::DiskANNCanonicalCoverage,
    format::{DiskANNCanonicalOrigin, DiskANNGeneration, DiskANNVectorVersion},
    pages::DiskANNOriginReader,
    DiskANNCanonicalCounts, DiskANNPopulationState,
};
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, KeyValueDiskANNCanonical,
    KeyValueDiskANNPopulationRecords as Layout, KeyValueDiskANNSource, RetainedDiskANNCanonical,
};
use uqa_storage::{RelationIdentity, StorageBackendResult};

const TABLE: &str = "public.publication";
const FIELD: &str = "vector";

#[path = "populations/failures.rs"]
mod failures;

fn field() -> Vec<u8> {
    let mut key = vec![b'v'];
    for value in [TABLE, FIELD] {
        key.extend_from_slice(&(value.len() as u32).to_be_bytes());
        key.extend_from_slice(value.as_bytes());
    }
    key
}

fn seed(persistence: &Arc<Persistence>) -> Arc<VersionedKeyValueStore> {
    let store = Arc::new(persistence.session(1 << 22));
    let erased: Arc<dyn KeyValueStore> = store.clone();
    let canonical = setup(&erased);
    let control = StorageReadControl::with_limit(1 << 20);
    canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
    canonical
        .replace(2, &[vec![0.0, 1.0], vec![0.0, 0.0]], &control)
        .unwrap();
    canonical.replace(3, &[], &control).unwrap();
    store
}

struct Built {
    coverage: DiskANNCanonicalCoverage<RetainedDiskANNCanonical>,
    sealed: Arc<KeyValueDiskANNSource>,
    origins: DiskANNOriginReader,
    generation: DiskANNGeneration,
    header: Vec<u8>,
    control: StorageReadControl,
}

impl Built {
    fn capture(store: &Arc<VersionedKeyValueStore>) -> Self {
        let erased: Arc<dyn KeyValueStore> = store.clone();
        let control = StorageReadControl::with_limit(1 << 20);
        let canonical = KeyValueDiskANNCanonical::new(erased.clone(), TABLE, FIELD, 2).unwrap();
        let source = canonical
            .retain_for_index(
                &RelationIdentity::new("public", "publication_idx"),
                &control,
            )
            .unwrap();
        let parameters = source.index_parameters().unwrap();
        let scope = source.index_scope(&Resolver, &control).unwrap();
        let repository = KeyValueDiskANNStore::connect(&erased, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        let coverage =
            build_diskann_publication_fixture(source, &mut stage, parameters, &control).unwrap();
        let generation = stage.generation();
        let sealed = repository.open_source(generation, &control).unwrap();
        let origins = DiskANNOriginReader::open(sealed.clone(), 8192, &control).unwrap();
        let header = Layout::header_key(&field(), generation, &control)
            .unwrap()
            .to_vec();
        Self {
            coverage,
            sealed,
            origins,
            generation,
            header,
            control,
        }
    }

    fn publish(
        &self,
        store: &VersionedKeyValueStore,
        previous: Option<&Self>,
    ) -> StorageBackendResult<()> {
        let template = DiskANNPopulationState::from_counts(
            self.generation,
            2,
            DiskANNCanonicalCounts::new(900, 700)?,
        )?
        .encode();
        store.with_mutation(&mut |read, batch| {
            RetainedDiskANNCanonical::publish_generation(
                &self.coverage,
                &Resolver,
                &self.sealed,
                read,
                batch,
                &self.control,
            )?;
            if let Some(previous) = previous {
                batch.retire_diskann_population(&previous.header)?;
            }
            batch.publish_diskann_population(&self.header, &template, self.origins.clone())
        })
    }

    fn counts(&self, store: &dyn KeyValueStore) -> (u64, u64) {
        let bytes = store.get(&self.header).unwrap().unwrap();
        let counts = DiskANNPopulationState::decode(&bytes, self.generation, 2)
            .unwrap()
            .counts();
        (counts.current_vectors(), counts.changed_vectors())
    }
}

fn replace(store: &VersionedKeyValueStore, document: u64, count: u64) -> StorageBackendResult<()> {
    let mut calls = 0;
    store.with_versioned_mutation(&mut |mutation, _, batch| {
        calls += 1;
        let mut prefix = field();
        prefix.extend_from_slice(&document.to_be_bytes());
        batch.delete_prefix(&prefix)?;
        for ordinal in 0..count {
            let mut key = prefix.clone();
            key.extend_from_slice(&ordinal.to_be_bytes());
            let mut raw = Vec::new();
            raw.extend_from_slice(&1.0_f32.to_le_bytes());
            raw.extend_from_slice(&0.0_f32.to_le_bytes());
            batch.put(&key, &raw)?;
        }
        let mut key = b"\0uqa-diskann-canonical-v1\0".to_vec();
        key.extend_from_slice(&prefix);
        let version = DiskANNVectorVersion::new(mutation.transaction(), mutation.revision())?;
        let origin = DiskANNCanonicalOrigin::new(version, 2, count)?;
        batch.replace_diskann_origin(&key, &origin.encode())
    })?;
    assert_eq!(calls, 1);
    Ok(())
}

#[test]
fn diskann_population_disjoint_writers_merge_in_both_orders_and_after_command_refresh() {
    for reverse in [false, true] {
        for refresh in [false, true] {
            let persistence = Persistence::new();
            let a = seed(&persistence);
            let built = Built::capture(&a);
            built.publish(&a, None).unwrap();
            assert_eq!(built.counts(&*a), (3, 0));
            let b = persistence.session(1 << 22);
            a.begin_transaction().unwrap();
            b.begin_transaction().unwrap();
            replace(&a, 1, 2).unwrap();
            replace(&b, 2, 1).unwrap();
            assert_eq!(built.counts(&*a), (4, 2));
            assert_eq!(built.counts(&b), (2, 1));
            let kept = a.record_snapshot().unwrap();
            let (first, second) = if reverse { (&b, &*a) } else { (&*a, &b) };
            first.commit_transaction().unwrap();
            if refresh {
                second
                    .refresh_transaction_snapshot(second.retention_control().cancellation())
                    .unwrap();
                assert_eq!(built.counts(second), (3, 3));
            }
            second.commit_transaction().unwrap();
            assert_eq!(built.counts(&*a), (3, 3));
            let held = kept.get(&built.header, &built.control).unwrap().unwrap();
            let counts = DiskANNPopulationState::decode(held.value().unwrap(), built.generation, 2)
                .unwrap()
                .counts();
            assert_eq!((counts.current_vectors(), counts.changed_vectors()), (4, 2));
        }
    }
}

#[test]
fn diskann_population_same_document_conflict_preserves_the_winning_counts() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let built = Built::capture(&a);
    built.publish(&a, None).unwrap();
    let b = persistence.session(1 << 22);
    a.begin_transaction().unwrap();
    b.begin_transaction().unwrap();
    replace(&a, 1, 2).unwrap();
    replace(&b, 1, 3).unwrap();
    a.commit_transaction().unwrap();
    assert!(b.commit_transaction().is_err());
    b.rollback_transaction().unwrap();
    assert_eq!(built.counts(&b), (4, 2));
}

#[test]
fn diskann_population_private_replacement_and_publication_follow_savepoint_undo() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let original = Built::capture(&a);
    original.publish(&a, None).unwrap();
    a.begin_transaction().unwrap();
    a.savepoint("before").unwrap();
    replace(&a, 1, 3).unwrap();
    replace(&a, 2, 0).unwrap();
    assert_eq!(original.counts(&*a), (3, 3));
    let rebuilt = Built::capture(&a);
    rebuilt.publish(&a, Some(&original)).unwrap();
    assert_eq!(rebuilt.counts(&*a), (3, 0));
    replace(&a, 3, 2).unwrap();
    assert_eq!(rebuilt.counts(&*a), (5, 2));
    let held = a.record_snapshot().unwrap();
    a.rollback_to_savepoint("before").unwrap();
    assert_eq!(original.counts(&*a), (3, 0));
    assert!(a.get(&rebuilt.header).unwrap().is_none());
    replace(&a, 2, 1).unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(original.counts(&*a), (2, 1));
    let held = held
        .get(&rebuilt.header, &rebuilt.control)
        .unwrap()
        .unwrap();
    let counts = DiskANNPopulationState::decode(held.value().unwrap(), rebuilt.generation, 2)
        .unwrap()
        .counts();
    assert_eq!((counts.current_vectors(), counts.changed_vectors()), (5, 2));
}

#[test]
fn diskann_population_rebuild_and_late_writers_reclassify_both_commit_orders() {
    for publish_first in [false, true] {
        let persistence = Persistence::new();
        let a = seed(&persistence);
        let original = Built::capture(&a);
        original.publish(&a, None).unwrap();
        let b = persistence.session(1 << 22);
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        let rebuilt = Built::capture(&a);
        rebuilt.publish(&a, Some(&original)).unwrap();
        replace(&b, 2, 3).unwrap();
        replace(&b, 4, 1).unwrap();
        if publish_first {
            a.commit_transaction().unwrap();
            b.commit_transaction().unwrap();
        } else {
            b.commit_transaction().unwrap();
            a.commit_transaction().unwrap();
        }
        assert_eq!(rebuilt.counts(&*a), (5, 4));
        assert!(a.get(&original.header).unwrap().is_none());
        let prefix = Layout
            .witness_prefix(&original.header, &original.control)
            .unwrap();
        assert!(a.scan_prefix(&prefix).unwrap().is_empty());
    }
}

#[test]
fn diskann_population_newer_build_can_be_published_on_an_older_canonical_view() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let original = Built::capture(&a);
    original.publish(&a, None).unwrap();
    let b = Arc::new(persistence.session(1 << 22));
    a.begin_transaction().unwrap();
    replace(&b, 2, 2).unwrap();
    let rebuilt = Built::capture(&b);
    rebuilt.publish(&a, Some(&original)).unwrap();
    assert_eq!(rebuilt.counts(&*a), (3, 2));
    a.commit_transaction().unwrap();
    assert_eq!(rebuilt.counts(&*a), (3, 0));
}

#[test]
fn diskann_population_first_publication_includes_a_previously_unbound_writer() {
    for publish_first in [false, true] {
        let persistence = Persistence::new();
        let a = seed(&persistence);
        let built = Built::capture(&a);
        let b = persistence.session(1 << 22);
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        replace(&b, 2, 3).unwrap();
        built.publish(&a, None).unwrap();
        if publish_first {
            a.commit_transaction().unwrap();
            b.commit_transaction().unwrap();
        } else {
            b.commit_transaction().unwrap();
            a.commit_transaction().unwrap();
        }
        assert_eq!(built.counts(&*a), (4, 3));
    }
}

#[test]
fn diskann_population_retirement_removes_witnesses_after_an_explicit_header_write() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let built = Built::capture(&a);
    built.publish(&a, None).unwrap();
    a.begin_transaction().unwrap();
    let header = a.get(&built.header).unwrap().unwrap();
    a.put(&built.header, &header).unwrap();
    replace(&a, 2, 3).unwrap();
    assert_eq!(built.counts(&*a), (4, 3));
    a.with_mutation(&mut |_, batch| batch.retire_diskann_population(&built.header))
        .unwrap();
    a.commit_transaction().unwrap();
    let prefix = Layout
        .witness_prefix(&built.header, &built.control)
        .unwrap();
    assert!(a.get(&built.header).unwrap().is_none());
    assert!(a.scan_prefix(&prefix).unwrap().is_empty());
}

#[test]
fn diskann_population_structural_preview_keeps_its_original_header_precondition() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let built = Built::capture(&a);
    built.publish(&a, None).unwrap();
    a.begin_transaction().unwrap();
    let header = a.get(&built.header).unwrap().unwrap();
    a.put(&built.header, &header).unwrap();
    replace(&a, 1, 2).unwrap();
    let b = persistence.session(1 << 22);
    b.put(b"unrelated", b"advance").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(built.counts(&*a), (4, 2));
    replace(&b, 2, 3).unwrap();
    assert!(a.commit_transaction().is_err());
    a.rollback_transaction().unwrap();
    assert_eq!(built.counts(&*a), (4, 3));
}

#[test]
fn diskann_population_effects_compose_with_vector_occurrence_and_graph_reconciliation() {
    use uqa_storage::{CatalogFacade, InvertedIndex, KeyValueCatalog, KeyValueInvertedIndex};

    for hnsw in [false, true] {
        for refresh in [false, true] {
            for rebuild in [false, true] {
                let persistence = Persistence::new();
                let (a, b, mut vector_left, mut vector_right) =
                    super::super::vector_merging::fixture(&persistence, hnsw);
                let erased: Arc<dyn KeyValueStore> = a.clone();
                let canonical = setup(&erased);
                canonical
                    .replace(
                        1,
                        &[vec![1.0, 0.0]],
                        &StorageReadControl::with_limit(1 << 20),
                    )
                    .unwrap();
                let built = Built::capture(&a);
                built.publish(&a, None).unwrap();
                let left = KeyValueCatalog::new(a.clone());
                let right = KeyValueCatalog::new(b.clone());
                left.save_named_graph("g").unwrap();
                left.save_vertex(1, "node", "{}").unwrap();
                left.save_graph_membership("vertex", 1, "g").unwrap();
                let mut text_left = KeyValueInvertedIndex::new(
                    a.clone(),
                    "docs",
                    uqa_analysis::whitespace_analyzer(),
                );
                let mut text_right = KeyValueInvertedIndex::new(
                    b.clone(),
                    "docs",
                    uqa_analysis::whitespace_analyzer(),
                );
                a.begin_transaction().unwrap();
                replace(&a, 1, 2).unwrap();
                let rebuilt = rebuild.then(|| {
                    let rebuilt = Built::capture(&a);
                    rebuilt.publish(&a, Some(&built)).unwrap();
                    rebuilt
                });
                let selected = rebuilt.as_ref().unwrap_or(&built);
                vector_left.add(11, vec![0.5, 0.5]).unwrap();
                text_left
                    .add_document(11, BTreeMap::from([("body".into(), "alpha alpha".into())]))
                    .unwrap();
                left.save_vertex(1, "node", "{\"changed\":true}").unwrap();
                replace(&b, 2, 1).unwrap();
                vector_right.add(12, vec![0.0, 1.0]).unwrap();
                text_right
                    .add_document(
                        12,
                        BTreeMap::from([("body".into(), "alpha alpha alpha".into())]),
                    )
                    .unwrap();
                if refresh {
                    a.refresh_transaction_snapshot(a.retention_control().cancellation())
                        .unwrap();
                    assert_eq!(selected.counts(&*a), (3, if rebuild { 1 } else { 3 }));
                }
                right.save_path_index("late", "[]").unwrap();
                right.finish_path_index_data("late", "g", "[]").unwrap();
                persistence.state.lock().commit_fault = CommitFault::ConcurrentCommit;
                a.commit_transaction().unwrap();
                assert_eq!(selected.counts(&*b), (3, if rebuild { 1 } else { 3 }));
                assert_eq!(vector_right.count().unwrap(), 3);
                assert_eq!(text_right.doc_count().unwrap(), 2);
                assert_eq!(text_right.total_field_length("body").unwrap(), 5);
                assert_eq!(
                    right.graph_vertex(1).unwrap().unwrap().properties_json,
                    "{\"changed\":true}"
                );
                assert!(!right.path_index_data_is_current("late", "[]").unwrap());
            }
        }
    }
}
