//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph consumers share redb's common transaction and independent reservation contracts.

use uqa_core::Vertex;
use uqa_graph::{GraphStore, LabelKind, PersistentGraphStore};
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;

#[test]
fn graph_identifier_sessions_commit_independently_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-identifiers.redb");
    let last = {
        let storage = RedbStorage::open(&path).unwrap();
        let a = storage.open_session().unwrap();
        let b = storage.open_session().unwrap();
        let mut left = PersistentGraphStore::from_catalog(a.catalog.clone(), a.backend.clone());
        let mut right = PersistentGraphStore::from_catalog(b.catalog.clone(), b.backend.clone());
        left.create_graph("g").unwrap();
        left.create_label("g", "item", LabelKind::Vertex).unwrap();
        a.backend.begin_transaction().unwrap();
        b.backend.begin_transaction().unwrap();
        let first = left.allocate_vertex_id("item", "g").unwrap();
        let second = right.allocate_vertex_id("item", "g").unwrap();
        assert_ne!(first, second);
        left.add_vertex(Vertex::new(first, "item"), "g").unwrap();
        right.add_vertex(Vertex::new(second, "item"), "g").unwrap();
        b.backend.commit_transaction().unwrap();
        assert!(left.get_vertex(second).unwrap().is_none());
        a.backend.commit_transaction().unwrap();
        assert_eq!(left.vertices_in_graph("g").unwrap().len(), 2);
        for data_wins in [false, true] {
            a.backend.begin_transaction().unwrap();
            b.backend.begin_transaction().unwrap();
            let id = left.allocate_vertex_id("item", "g").unwrap();
            left.add_vertex(Vertex::new(id, "item"), "g").unwrap();
            right
                .create_label("g", "conflict", LabelKind::Edge)
                .unwrap();
            let (winner, loser) = if data_wins { (&a, &b) } else { (&b, &a) };
            winner.backend.commit_transaction().unwrap();
            assert!(loser.backend.commit_transaction().is_err());
            loser.backend.rollback_transaction().unwrap();
            assert_eq!(left.get_vertex(id).unwrap().is_some(), data_wins);
            left.drop_label("g", "conflict").unwrap();
        }
        a.backend.begin_transaction().unwrap();
        let undone = left.allocate_vertex_id("item", "g").unwrap();
        a.backend.rollback_transaction().unwrap();
        assert!(right.allocate_vertex_id("item", "g").unwrap() > undone);
        left.next_vertex_id().unwrap()
    };
    let storage = RedbStorage::open(&path).unwrap();
    let session = storage.open_session().unwrap();
    let mut graph = PersistentGraphStore::from_catalog(session.catalog, session.backend);
    assert!(graph.allocate_vertex_id("item", "g").unwrap() > last);
    graph.clear().unwrap();
    assert_eq!(graph.next_vertex_id().unwrap(), 1);
}
