//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::destination;
use super::*;
use crate::tests::providers::Provider;
use std::collections::BTreeSet;
use std::path::Path;
use uqa_core::{Edge, Vertex};

pub(super) fn create_graph_with_edge(engine: &Engine, graph: &str, first: u64) {
    assert!(engine.create_graph(graph).unwrap());
    engine
        .add_graph_vertex(Vertex::new(first, "Node"), graph)
        .unwrap();
    engine
        .add_graph_vertex(Vertex::new(first + 1, "Node"), graph)
        .unwrap();
    engine
        .add_graph_edge(Edge::new(100 + first, first, first + 1, "edge"), graph)
        .unwrap();
}

fn assert_pairs(engine: &Engine, graph: &str, index: &str, expected: &[(u64, u64)]) {
    let actual = engine
        .get_path_index(index, graph)
        .unwrap()
        .expect("the graph owns this index")
        .lookup(&["edge".to_owned()])
        .unwrap()
        .expect("the index contains this label sequence");
    assert_eq!(actual, expected.iter().copied().collect::<BTreeSet<_>>());
}

fn assert_both_indexes(engine: &Engine, company_pairs: &[(u64, u64)]) {
    let listed = engine.list_path_indexes().unwrap();
    assert_eq!(listed.len(), 2);
    assert_ne!(listed[0], listed[1]);
    assert_pairs(engine, "company", "archive::idx", company_pairs);
    assert_pairs(engine, "company::archive", "idx", &[(10, 11)]);

    engine
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    engine.list_graphs().unwrap();
    assert_pairs(engine, "company", "archive::idx", company_pairs);
    assert_pairs(engine, "company::archive", "idx", &[(10, 11)]);
    engine.sql("ROLLBACK", &[]).unwrap();
}

fn restore_graphs(
    provider: Provider,
    path: &Path,
    repository: &LocalRepository,
    root: BackupRoot,
    control: &StorageReadControl,
) -> RestoredEngine {
    let mut job = Engine::new()
        .begin_restore(
            repository,
            root.backup(),
            destination(provider, path),
            RestoreOptions::new(16 << 20, 8 << 20),
            control,
        )
        .unwrap();
    prepare(&mut job);
    job.publish().unwrap()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn graph_path_indexes_preserve_their_owners_after_source_retirement(
    #[case] provider: Provider,
    #[values(false, true)] incremental: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let source_directory = directory.path().join("source");
    std::fs::create_dir(&source_directory).unwrap();
    let database = source_directory.join("database");
    let source = provider.open(&database);
    let repository = repository(&directory.path().join("repository"));
    let control = StorageReadControl::with_limit(32 << 20);
    let sequence = [vec!["edge".to_owned()]];

    create_graph_with_edge(&source, "company", 1);
    create_graph_with_edge(&source, "company::archive", 10);
    assert!(source
        .build_path_index("archive::idx", "company", &sequence)
        .unwrap());
    assert!(source
        .build_path_index("idx", "company::archive", &sequence)
        .unwrap());
    assert_both_indexes(&source, &[(1, 2)]);
    let parent = capture(&source, &repository, None);
    let mut expected = vec![(1, 2)];
    let root = if incremental {
        source
            .add_graph_edge(Edge::new(201, 2, 1, "edge"), "company")
            .unwrap();
        assert_pairs(&source, "company::archive", "idx", &[(10, 11)]);
        assert!(source
            .build_path_index("archive::idx", "company", &sequence)
            .unwrap());
        expected.push((2, 1));
        capture(&source, &repository, Some(parent.backup()))
    } else {
        parent
    };
    drop(source);
    if !matches!(provider, Provider::Memory) {
        assert_both_indexes(&provider.open(&database), &expected);
    }
    std::fs::remove_dir_all(source_directory).unwrap();
    if incremental {
        retire_ancestor(&repository, parent, &control);
    }

    let path = directory.path().join("restored");
    let restored = restore_graphs(provider, &path, &repository, root, &control);
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_both_indexes(&restored, &expected);

    assert!(restored.drop_path_index("archive::idx", "company").unwrap());
    assert_pairs(&restored, "company::archive", "idx", &[(10, 11)]);
    assert!(restored
        .build_path_index("archive::idx", "company", &sequence)
        .unwrap());
    assert!(restored.drop_graph("company").unwrap());
    assert_eq!(restored.list_path_indexes().unwrap().len(), 1);
    assert_pairs(&restored, "company::archive", "idx", &[(10, 11)]);
    drop(restored);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(reopened.list_path_indexes().unwrap().len(), 1);
        assert_pairs(&reopened, "company::archive", "idx", &[(10, 11)]);
    }
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn empty_path_index_names_and_adjacent_colons_round_trip(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("source");
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&database);
    create_graph_with_edge(&source, "g", 1);
    create_graph_with_edge(&source, "g:", 10);
    let registrations = [
        ("g", "", 1),
        ("g", "ordinary", 1),
        ("g", ":c", 1),
        ("g:", "c", 10),
    ];
    for (graph, name, _) in registrations {
        assert!(source
            .build_path_index(name, graph, &[vec!["edge".into()]])
            .unwrap());
    }
    let check = |engine: &Engine| {
        let listed = engine.list_path_indexes().unwrap();
        assert_eq!(listed.len(), registrations.len());
        assert!(listed.iter().any(|key| key == "g::"));
        assert!(listed.iter().any(|key| key == "g::ordinary"));
        for (graph, name, first) in registrations {
            assert_pairs(engine, graph, name, &[(first, first + 1)]);
        }
    };
    check(&source);
    let root = capture(&source, &repository, None);
    drop(source);
    if !matches!(provider, Provider::Memory) {
        check(&provider.open(&database));
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let restored = restore_graphs(
        provider,
        &directory.path().join("restored"),
        &repository,
        root,
        &control,
    );
    check(&restored);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
