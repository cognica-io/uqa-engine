//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable and retained path reads participate in actual concurrent commit histories.

#[path = "paths/session.rs"]
mod session;

use std::{collections::BTreeSet, error::Error};
use uqa_core::{CancellationToken, Edge, Value, Vertex};
use uqa_graph::{GraphStore, PathIndex, PersistentGraphStore};
use uqa_storage::mvcc::{
    SerializableKeySpace, SerializablePredicate, SerializableReadContext, VersionError,
};
use uqa_storage::{PersistentStorageSession, StorageBackendError};

fn store(session: &PersistentStorageSession) -> PersistentGraphStore {
    PersistentGraphStore::from_catalog(session.catalog.clone(), session.backend.clone())
}

fn setup(session: &PersistentStorageSession) {
    let mut graph = store(session);
    for name in ["g", "other"] {
        graph.create_graph(name).unwrap();
        for id in [1, 2] {
            graph.add_vertex(Vertex::new(id, "P"), name).unwrap();
        }
    }
    graph.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
}

fn begin(session: &PersistentStorageSession) -> SerializableReadContext {
    session.backend.begin_transaction().unwrap();
    session
        .backend
        .serializable_session()
        .unwrap()
        .establish_serializable_snapshot()
        .unwrap()
}

fn pivot(a: &PersistentStorageSession, b: &SerializableReadContext) {
    let key = SerializablePredicate::point([7; 16], SerializableKeySpace::Rows, b"pivot");
    b.observe_read(key, &b.read_control(&CancellationToken::new()))
        .unwrap();
    a.backend
        .serializable_session()
        .unwrap()
        .observe_serializable_write(key)
        .unwrap();
    a.catalog.set_metadata("pivot", "changed").unwrap();
}

fn has_cause(
    mut error: &(dyn Error + 'static),
    check: impl Fn(&(dyn Error + 'static)) -> bool,
) -> bool {
    loop {
        if check(error) {
            return true;
        }
        let Some(source) = error.source() else {
            return false;
        };
        error = source;
    }
}

fn finish(a: &PersistentStorageSession, b: &PersistentStorageSession, conflict: bool) {
    a.backend.commit_transaction().unwrap();
    if conflict {
        let error = b.backend.commit_transaction().unwrap_err();
        assert!(
            has_cause(&error, |cause| matches!(
                cause.downcast_ref::<VersionError>(),
                Some(VersionError::SerializationConflict { .. })
            )),
            "{error}"
        );
        b.backend.rollback_transaction().unwrap();
    } else {
        b.backend.commit_transaction().unwrap();
    }
}

fn build(session: &PersistentStorageSession, sequence: &[String]) -> PathIndex {
    PathIndex::build_persistent(
        session.catalog.clone(),
        session.backend.clone(),
        "g::p",
        "g",
        &[sequence.to_vec()],
    )
    .unwrap()
}

#[test]
fn durable_cached_and_invalidated_paths_observe_empty_results_without_crossing_labels_or_graphs() {
    for cached in [true, false] {
        for mutation in [
            "selected",
            "other label",
            "other graph",
            "properties",
            "unused",
        ] {
            let database = session::Database::new();
            let a = database.session();
            let b = database.session();
            setup(&a);
            if mutation == "properties" {
                store(&a)
                    .add_edge(Edge::new(11, 1, 2, "likes"), "g")
                    .unwrap();
            }
            let sequence = vec!["likes".into()];
            let index = build(&a, &sequence);
            if !cached {
                a.catalog.clear_path_index_data("g::p").unwrap();
            }
            begin(&a);
            let rb = begin(&b);
            assert_eq!(
                a.catalog
                    .path_index_data_is_current("g::p", "[[\"likes\"]]")
                    .unwrap(),
                cached,
            );
            if mutation == "unused" {
                assert!(index.lookup(&["missing".into()]).unwrap().is_none());
            } else {
                assert_eq!(
                    index.lookup(&sequence).unwrap().unwrap().len(),
                    usize::from(mutation == "properties"),
                );
            }
            pivot(&a, &rb);
            if mutation == "properties" {
                let mut edge = Edge::new(11, 1, 2, "likes");
                edge.properties.insert("value".into(), Value::Int(2));
                store(&b).add_edge(edge, "g").unwrap();
            } else {
                store(&b)
                    .add_edge(
                        Edge::new(
                            11,
                            1,
                            2,
                            if mutation == "other label" {
                                "different"
                            } else {
                                "likes"
                            },
                        ),
                        if mutation == "other graph" {
                            "other"
                        } else {
                            "g"
                        },
                    )
                    .unwrap();
            }
            finish(&a, &b, mutation == "selected");
        }
    }
}

#[test]
fn cached_identity_paths_include_new_starting_vertices() {
    let database = session::Database::new();
    let a = database.session();
    let b = database.session();
    setup(&a);
    let index = build(&a, &[]);
    begin(&a);
    let rb = begin(&b);
    assert_eq!(
        index.lookup(&[]).unwrap().unwrap(),
        BTreeSet::from([(1, 1), (2, 2)])
    );
    pivot(&a, &rb);
    store(&b).add_vertex(Vertex::new(3, "P"), "g").unwrap();
    finish(&a, &b, true);
}

#[test]
fn durable_path_builds_observe_their_original_graph_reads() {
    let database = session::Database::new();
    let a = database.session();
    let b = database.session();
    setup(&a);
    begin(&a);
    let rb = begin(&b);
    let _index = build(&a, &["likes".into()]);
    pivot(&a, &rb);
    store(&b)
        .add_edge(Edge::new(11, 1, 2, "likes"), "g")
        .unwrap();
    finish(&a, &b, true);
}

#[test]
fn retained_durable_paths_keep_original_participants_and_reader_cancellation() {
    let database = session::Database::new();
    let a = database.session();
    let b = database.session();
    setup(&a);
    let sequence = vec!["likes".into()];
    let index = build(&a, &sequence);
    begin(&a);
    let cancellation = CancellationToken::new();
    let retained = a.backend.open_retained_read_session(&cancellation).unwrap();
    let index = index
        .rebind_persistent(retained.catalog, retained.backend)
        .unwrap();
    let rb = begin(&b);
    assert!(index.lookup(&sequence).unwrap().unwrap().is_empty());
    pivot(&a, &rb);
    store(&b)
        .add_edge(Edge::new(11, 1, 2, "likes"), "g")
        .unwrap();
    finish(&a, &b, true);
    begin(&a);
    assert!(index.lookup(&sequence).is_err());
    a.backend.rollback_transaction().unwrap();

    begin(&a);
    let retained = a.backend.open_retained_read_session(&cancellation).unwrap();
    let index = index
        .rebind_persistent(retained.catalog, retained.backend)
        .unwrap();
    cancellation.cancel();
    let error = index.lookup(&sequence).unwrap_err();
    assert!(
        has_cause(&error, |cause| matches!(
            cause.downcast_ref::<StorageBackendError>(),
            Some(StorageBackendError::Cancelled(_))
        )),
        "{error}"
    );
    a.backend.rollback_transaction().unwrap();
}

#[test]
fn retained_cached_paths_share_the_original_observation_allowance() {
    let database = session::Database::new();
    let a = database.session();
    setup(&a);
    let sequence = vec!["likes".into()];
    let index = build(&a, &sequence);
    let context = begin(&a);
    let retained = a
        .backend
        .open_retained_read_session(&CancellationToken::new())
        .unwrap();
    let index = index
        .rebind_persistent(retained.catalog, retained.backend)
        .unwrap();
    let control = context.read_control(&CancellationToken::new());
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let error = index.lookup(&sequence).unwrap_err();
    assert!(
        has_cause(&error, |cause| matches!(
            cause.downcast_ref::<StorageBackendError>(),
            Some(StorageBackendError::Memory(_))
        )),
        "{error}"
    );
    drop(held);
    assert!(index.lookup(&sequence).unwrap().unwrap().is_empty());
    a.backend.rollback_transaction().unwrap();
}

#[test]
fn cached_path_observations_do_not_hydrate_unselected_entity_properties() {
    let database = session::Database::new();
    let a = database.session();
    setup(&a);
    let sequence = vec!["knows".into()];
    let index = build(&a, &sequence);
    a.catalog
        .save_edge(10, 1, 2, "knows", "invalid unused property bytes")
        .unwrap();
    a.catalog
        .finish_path_index_data("g::p", "g", "[[\"knows\"]]")
        .unwrap();
    begin(&a);
    assert!(store(&a).get_edge(10).is_err());
    assert_eq!(
        index.lookup(&sequence).unwrap().unwrap(),
        BTreeSet::from([(1, 2)])
    );
    a.backend.rollback_transaction().unwrap();
}

#[test]
fn retained_path_data_stays_fixed_without_serializable_admission() {
    let database = session::Database::new();
    let a = database.session();
    setup(&a);
    let sequence = vec!["likes".into()];
    let index = build(&a, &sequence);
    let retained = a
        .backend
        .open_retained_read_session(&CancellationToken::new())
        .unwrap();
    let fixed = index
        .rebind_persistent(retained.catalog, retained.backend)
        .unwrap();
    store(&a)
        .add_edge(Edge::new(11, 1, 2, "likes"), "g")
        .unwrap();
    assert!(fixed.lookup(&sequence).unwrap().unwrap().is_empty());
    assert_eq!(
        index.lookup(&sequence).unwrap().unwrap(),
        BTreeSet::from([(1, 2)])
    );
}
