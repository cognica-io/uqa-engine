//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph topology races use the same contract in native catalog, standalone and byte-store sessions.

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::GraphStore;
use uqa_storage::{mvcc::VersionedSessionOptions, PersistentStorageBackend};
use uqa_storage_sqlite::{SQLiteGraphStore, SQLiteStorageBackend};

use super::{
    graph_identifiers::{graph, session},
    open, MODES,
};

fn verify<G: GraphStore>(
    left: &mut G,
    right: &mut G,
    a: &dyn PersistentStorageBackend,
    b: &dyn PersistentStorageBackend,
) {
    for shared in [false, true] {
        for reference_wins in [false, true] {
            let slot = 2 * u64::from(shared) + u64::from(reference_wins);
            let name = format!("g_{slot}");
            let other = format!("other_{slot}");
            let source = 10 * slot + 1;
            let target = source + 1;
            let edge = 100 + slot;
            left.create_graph(&name).unwrap();
            left.add_vertex(Vertex::new(source, "node"), &name).unwrap();
            left.add_vertex(Vertex::new(target, "node"), &name).unwrap();
            if shared {
                left.create_graph(&other).unwrap();
                left.add_vertex(Vertex::new(source, "node"), &other)
                    .unwrap();
            }
            a.begin_transaction().unwrap();
            b.begin_transaction().unwrap();
            left.add_edge(Edge::new(edge, source, target, "link"), &name)
                .unwrap();
            right.remove_vertex(source, &name).unwrap();
            let (winner, loser) = if reference_wins { (a, b) } else { (b, a) };
            winner.commit_transaction().unwrap();
            assert!(
                loser.commit_transaction().is_err(),
                "shared={shared}, reference_wins={reference_wins}"
            );
            loser.rollback_transaction().unwrap();
            assert_eq!(
                left.edges_in_graph(&name).unwrap().len(),
                usize::from(reference_wins)
            );
            assert_eq!(
                right.vertex_graphs(source).unwrap().contains(&name),
                reference_wins
            );
            assert_eq!(
                right.get_vertex(source).unwrap().is_some(),
                shared || reference_wins
            );
        }
    }
    left.create_graph("parallel").unwrap();
    for id in [1001, 1002] {
        left.add_vertex(Vertex::new(id, "node"), "parallel")
            .unwrap();
    }
    a.begin_transaction().unwrap();
    b.begin_transaction().unwrap();
    left.add_edge(Edge::new(1001, 1001, 1002, "link"), "parallel")
        .unwrap();
    right
        .add_edge(Edge::new(1002, 1001, 1002, "link"), "parallel")
        .unwrap();
    b.commit_transaction().unwrap();
    a.commit_transaction().unwrap();
    for property_wins in [false, true] {
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        left.add_edge(
            Edge::new(1003 + u64::from(property_wins), 1001, 1002, "link"),
            "parallel",
        )
        .unwrap();
        let mut vertex = Vertex::new(1001, "node");
        vertex
            .properties
            .insert("updated".into(), Value::Bool(property_wins));
        right.add_vertex(vertex, "parallel").unwrap();
        let (first, second) = if property_wins { (b, a) } else { (a, b) };
        first.commit_transaction().unwrap();
        second.commit_transaction().unwrap();
    }
    assert_eq!(left.edges_in_graph("parallel").unwrap().len(), 4);
}

#[test]
fn graph_lifetimes_coordinate_endpoints_without_serializing_properties_in_every_mode() {
    for mode in MODES {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("graph-lifetimes.db");
            let a = session(mode, &path, native, true);
            let b = session(mode, &path, native, false);
            verify(
                &mut graph(&a),
                &mut graph(&b),
                a.backend.as_ref(),
                b.backend.as_ref(),
            );
            drop((a, b));
            let reopened = session(mode, &path, native, false);
            assert_eq!(
                graph(&reopened).edges_in_graph("parallel").unwrap().len(),
                4
            );
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("standalone-lifetimes.db");
        let a = open(mode, &path);
        let mut left = SQLiteGraphStore::open(a.clone(), Some("direct")).unwrap();
        a.bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let b = open(mode, &path);
        b.bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut right = SQLiteGraphStore::open(b.clone(), Some("direct")).unwrap();
        verify(
            &mut left,
            &mut right,
            &SQLiteStorageBackend::new(a.clone()),
            &SQLiteStorageBackend::new(b.clone()),
        );
        drop((left, right, a, b));
        let reopened = open(mode, &path);
        reopened
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let store = SQLiteGraphStore::open(reopened, Some("direct")).unwrap();
        assert_eq!(store.edges_in_graph("parallel").unwrap().len(), 4);
    }
}
