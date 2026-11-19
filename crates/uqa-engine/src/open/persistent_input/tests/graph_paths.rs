//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;
use uqa_core::{Edge, Vertex};

const GRAPH: &str = "catalog::graph";

fn create_graph(source: &Engine) {
    assert!(source.create_graph(GRAPH).unwrap());
    for id in [1, 2] {
        source
            .add_graph_vertex(Vertex::new(id, "V"), GRAPH)
            .unwrap();
    }
    source
        .add_graph_edge(Edge::new(3, 1, 2, "edge"), GRAPH)
        .unwrap();
}

fn register(source: &Engine, names: std::ops::Range<usize>) {
    source.sql("BEGIN", &[]).unwrap();
    for index in names {
        assert!(source
            .build_path_index(&format!("p{index:03}"), GRAPH, &[vec!["edge".into()]])
            .unwrap());
    }
    source.sql("COMMIT", &[]).unwrap();
}

fn reach_registries(input: &mut PersistentEngineInput) {
    for _ in 0..65_536 {
        if matches!(
            &input.candidate.as_ref().unwrap().stage,
            Stage::Registries(_)
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("the private registry stage was not reached");
}

fn registry_steps(source: &Engine, count: usize) -> usize {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(source, &control);
    reach_registries(&mut input);
    for steps in 0..262_144 {
        let candidate = input.candidate.as_ref().unwrap();
        let paths = candidate.session.engine().durable.path_indexes.read();
        if matches!(&candidate.stage, Stage::Caches(_)) {
            assert_eq!(paths.len(), count);
            drop(paths);
            let restored = finish(&mut input).finish().unwrap();
            let index = restored.get_path_index("p000", GRAPH).unwrap().unwrap();
            assert_eq!(
                index.lookup(&["edge".into()]).unwrap(),
                Some(BTreeSet::from([(1, 2)]))
            );
            drop((index, restored, input));
            assert_eq!(control.memory().used(), 0);
            return steps;
        }
        assert_eq!(
            paths.len(),
            0,
            "the partially loaded path registry became visible"
        );
        drop(paths);
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("the path registry did not finish");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_path_registries_yield_between_definitions_before_publication(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    create_graph(&source);
    register(&source, 0..1);
    let one = registry_steps(&source, 1);
    register(&source, 1..33);
    let many = registry_steps(&source, 33);
    assert!(
        many >= one + 32,
        "{provider:?}: one index took {one} steps and 33 took {many}"
    );
}

fn start_paths(source: &Engine, control: &StorageReadControl) -> PersistentEngineInput {
    let mut input = input(source, control);
    reach_registries(&mut input);
    for _ in 0..65_536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Registries(RegistryInput::PathIndexes(_))
        ) {
            break;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    for _ in 0..8 {
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Registries(RegistryInput::PathIndexes(_))
        ));
        assert!(input.advance(1).unwrap().is_none());
    }
    input
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_path_registries_keep_selected_pairs_and_then_accept_later_writes(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    create_graph(&source);
    register(&source, 0..8);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = start_paths(&source, &control);
    assert!(source.drop_path_index("p000", GRAPH).unwrap());
    register(&source, 8..9);
    let session = finish(&mut input);
    {
        let paths = session.engine().durable.path_indexes.read();
        let key = |name| uqa_storage::catalog::path_index::key(GRAPH, name);
        assert_eq!(paths.len(), 8);
        assert!(!paths.contains_key(&key("p008")));
        let selected = paths.get(&key("p000")).unwrap();
        assert_eq!(
            selected.lookup(&["edge".into()]).unwrap(),
            Some(BTreeSet::from([(1, 2)]))
        );
    }
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    assert!(restored.get_path_index("p000", GRAPH).unwrap().is_none());
    assert!(restored.get_path_index("p008", GRAPH).unwrap().is_some());
    register(&restored, 9..10);
    assert!(restored.get_path_index("p009", GRAPH).unwrap().is_some());
    drop((restored, input, occupied));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_path_registries_release_partial_bindings_and_the_read_transaction(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    create_graph(&source);
    register(&source, 0..8);
    let expected = source.list_path_indexes().unwrap();
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = start_paths(&source, &control);
        let session = input.candidate.as_ref().unwrap().session.engine();
        let backend = Arc::clone(session.storage.backend.as_ref().unwrap());
        assert!(backend.in_transaction());
        assert!(session.durable.path_indexes.read().is_empty());
        let occupied = if cancel {
            control.cancellation().cancel();
            None
        } else {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        };
        let error = loop {
            match input.advance(1) {
                Ok(None) => assert!(matches!(
                    input.candidate.as_ref().unwrap().stage,
                    Stage::Registries(RegistryInput::PathIndexes(_))
                )),
                Ok(Some(_)) => panic!("failed path registry was published"),
                Err(error) => break error,
            }
        };
        if cancel {
            assert!(
                matches!(error, StorageBackendError::Cancelled(_)),
                "{error}"
            );
        } else {
            assert!(matches!(error, StorageBackendError::Memory(_)), "{error}");
        }
        assert!(!backend.in_transaction());
        assert!(input.candidate.is_none());
        drop((input, occupied));
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(source.list_path_indexes().unwrap(), expected);
    register(&source, 8..9);
}
