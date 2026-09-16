//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public native graph reads select identities and hydrate requested records on the logical session boundary.

use uqa_storage::{GraphEntityFilter, GraphEntityKind};

use super::*;

fn filter(kind: GraphEntityKind, graph: Option<&str>) -> GraphEntityFilter<'_> {
    GraphEntityFilter::new(kind, graph)
}

fn check_catalog(catalog: &Catalog) {
    use GraphEntityKind::{Edge, Vertex};
    assert!(catalog.named_graph_exists("g").unwrap());
    assert!(!catalog.named_graph_exists("missing").unwrap());
    assert_eq!(catalog.load_named_graphs().unwrap(), ["g"]);
    let vertex = catalog.graph_vertex(1).unwrap().unwrap();
    assert_eq!(
        (
            vertex.vertex_id,
            vertex.label.as_str(),
            vertex.properties_json.as_str()
        ),
        (1, "node", "{}")
    );
    let edge = catalog.graph_edge(9).unwrap().unwrap();
    assert_eq!(
        (
            edge.edge_id,
            edge.source_id,
            edge.target_id,
            edge.label.as_str(),
            edge.properties_json.as_str()
        ),
        (9, 1, 2, "rel", "{}")
    );
    assert!(catalog.graph_vertex(99).unwrap().is_none());
    assert!(catalog.graph_edge(99).unwrap().is_none());
    assert_eq!(
        catalog
            .load_vertices()
            .unwrap()
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        catalog
            .load_edges()
            .unwrap()
            .iter()
            .map(|row| row.edge_id)
            .collect::<Vec<_>>(),
        [9, 10]
    );
    assert_eq!(
        catalog.load_graph_memberships().unwrap(),
        [
            ("edge".into(), 9, "g".into()),
            ("vertex".into(), 1, "g".into())
        ]
    );
    assert_eq!(catalog.graph_entity_max_id(Vertex).unwrap(), Some(2));
    assert_eq!(catalog.graph_entity_max_id(Edge).unwrap(), Some(10));
    assert_eq!(catalog.graph_entity_memberships(Edge, 9).unwrap(), ["g"]);
    assert!(catalog
        .graph_entity_memberships(Edge, 10)
        .unwrap()
        .is_empty());
    assert!(catalog.graph_has_membership(Vertex, 1, "g").unwrap());
    assert!(!catalog.graph_has_membership(Vertex, 2, "g").unwrap());
    let snapshot = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
    assert_eq!(
        snapshot
            .vertices
            .iter()
            .map(|row| row.vertex_id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        snapshot
            .edges
            .iter()
            .map(|row| row.edge_id)
            .collect::<Vec<_>>(),
        [9]
    );
    assert_eq!(snapshot.label_registry_json, "registry");
    assert!(catalog
        .load_named_graph_snapshot("missing")
        .unwrap()
        .is_none());
    check_filters(catalog);
}

fn check_filters(catalog: &Catalog) {
    use GraphEntityKind::{Edge, Vertex};
    for kind in [Vertex, Edge] {
        for graph in [None, Some("g"), Some("missing")] {
            for label in [None, Some("node"), Some("rel"), Some("")] {
                let mut selected = filter(kind, graph);
                selected.label = label;
                let available = if kind == Vertex { [1, 2] } else { [9, 10] };
                let expected: Vec<_> = available
                    .into_iter()
                    .filter(|&id| graph.is_none() || (graph == Some("g") && (id == 1 || id == 9)))
                    .filter(|_| {
                        label.is_none()
                            || label == Some(if kind == Vertex { "node" } else { "rel" })
                    })
                    .collect();
                assert_eq!(
                    catalog.graph_entity_ids(selected, None, 16).unwrap(),
                    expected
                );
                assert_eq!(
                    catalog.graph_entity_count(selected).unwrap(),
                    expected.len() as u64
                );
            }
        }
    }
    for source in [None, Some(1), Some(2)] {
        for target in [None, Some(1), Some(2)] {
            let mut selected = filter(Edge, None);
            selected.source = source;
            selected.target = target;
            let expected = if source.is_none_or(|id| id == 1) && target.is_none_or(|id| id == 2) {
                vec![9, 10]
            } else {
                vec![]
            };
            assert_eq!(
                catalog.graph_entity_ids(selected, None, 16).unwrap(),
                expected
            );
            assert_eq!(
                catalog.graph_entity_count(selected).unwrap(),
                expected.len() as u64
            );
        }
    }
    assert_eq!(
        catalog
            .graph_entity_ids(filter(Edge, None), Some(9), 1)
            .unwrap(),
        [10]
    );
    assert!(catalog
        .graph_entity_ids(filter(Edge, None), Some(10), 1)
        .unwrap()
        .is_empty());
}

#[test]
fn native_graph_reads_match_the_legacy_catalog_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("graph-reads-{mode}.db"));
        let connection = connection(&path, mode);
        seed(&connection);
        with(&connection, |connection| {
            connection.execute(
                "INSERT INTO _metadata VALUES ('graph_label_registry::g', 'registry')",
                [],
            )?;
            Ok(())
        });
        let catalog = Catalog::open(connection.clone()).unwrap();
        check_catalog(&catalog);
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        check_catalog(&catalog);
        let reopened = super::connection(&path, mode);
        reopened
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        check_catalog(&Catalog::open(reopened).unwrap());
    }
}

#[test]
fn native_graph_identity_filters_skip_large_properties_and_unrelated_keys() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    seed(&connection);
    with(&connection, |connection| {
        connection.execute(
            "UPDATE _graph_vertices SET properties_json = ?1 WHERE vertex_id = 2",
            ["x".repeat(1 << 20)],
        )?;
        connection.execute(
            "UPDATE _graph_edges SET properties_json = ?1 WHERE edge_id = 10",
            ["x".repeat(1 << 20)],
        )?;
        connection.execute(
            "INSERT INTO _graph_vertices VALUES (3, ?1, '{}')",
            ["y".repeat(1 << 20)],
        )?;
        Ok(())
    });
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 25)).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 16 << 10,
        })
        .unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let retained = connection.native_snapshot().unwrap().unwrap();
    let baseline = retained.control.memory().used();
    let mut selected = filter(GraphEntityKind::Edge, Some("g"));
    selected.source = Some(1);
    selected.target = Some(2);
    selected.label = Some("rel");
    assert_eq!(catalog.graph_entity_ids(selected, None, 1).unwrap(), [9]);
    assert_eq!(catalog.graph_entity_count(selected).unwrap(), 1);
    selected.graph = None;
    assert_eq!(
        catalog.graph_entity_ids(selected, Some(9), 1).unwrap(),
        [10]
    );
    assert_eq!(catalog.graph_entity_count(selected).unwrap(), 2);
    let mut vertices = filter(GraphEntityKind::Vertex, None);
    vertices.label = Some("node");
    assert_eq!(
        catalog.graph_entity_ids(vertices, None, 16).unwrap(),
        [1, 2]
    );
    assert_eq!(
        catalog
            .graph_entity_max_id(GraphEntityKind::Vertex)
            .unwrap(),
        Some(3)
    );
    assert_eq!(catalog.graph_vertex(1).unwrap().unwrap().label, "node");
    assert!(catalog.graph_vertex(2).is_err());
    assert!(catalog.graph_edge(10).is_err());
    let graph = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
    assert_eq!(graph.vertices.len(), 1);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(retained.control.memory().used(), baseline);
}

fn stage(connection: &ManagedConnection, deletes: &[&NativeRecord], puts: &[&NativeRecord]) {
    connection
        .with_native_write(|_, batch| {
            for row in deletes {
                batch.delete(row.key())?;
            }
            for row in puts {
                batch.put(row.key(), row.row())?;
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
}

#[test]
fn native_graph_readers_observe_private_savepoints_and_preserve_another_commit() {
    for ending in ["commit", "rollback", "savepoint"] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let vertices = records(&connection, &store, Family::GraphVertices, &control);
        let edges = records(&connection, &store, Family::GraphEdges, &control);
        let node = lookup(&store, ("label", "node", 0, "vertex", 1), &control);
        let alpha = lookup(&store, ("label", "alpha", 0, "vertex", 1), &control);
        let beta = lookup(&store, ("label", "beta", 0, "vertex", 1), &control);
        let first = replace(&vertices[0], 1, ValueRef::Text(b"alpha"), &control);
        let last = replace(&vertices[0], 1, ValueRef::Text(b"beta"), &control);
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let other = connection.new_session();
        let a = Catalog::open(connection.clone()).unwrap();
        let b = Catalog::open(other.clone()).unwrap();
        connection.begin_transaction().unwrap();
        stage(&connection, &[&node], &[&first, &alpha]);
        connection.savepoint("first").unwrap();
        stage(&connection, &[&alpha], &[&last, &beta]);
        let old_source = lookup(&store, ("source", "", 1, "edge", 10), &control);
        let new_source = lookup(&store, ("source", "", 2, "edge", 10), &control);
        let changed = replace(&edges[1], 1, ValueRef::Integer(2), &control);
        stage(&other, &[&old_source], &[&changed, &new_source]);
        assert_eq!(a.graph_vertex(1).unwrap().unwrap().label, "beta");
        assert_eq!(a.graph_edge(10).unwrap().unwrap().source_id, 1);
        assert_eq!(b.graph_vertex(1).unwrap().unwrap().label, "node");
        assert_eq!(b.graph_edge(10).unwrap().unwrap().source_id, 2);
        let private = a.load_named_graph_snapshot("g").unwrap().unwrap();
        assert_eq!(private.vertices[0].label, "beta");
        let expected = match ending {
            "rollback" => {
                connection.rollback_transaction().unwrap();
                "node"
            }
            "savepoint" => {
                connection.rollback_to_savepoint("first").unwrap();
                assert_eq!(a.graph_vertex(1).unwrap().unwrap().label, "alpha");
                connection.commit_transaction().unwrap();
                "alpha"
            }
            _ => {
                connection.commit_transaction().unwrap();
                "beta"
            }
        };
        let mut selected = filter(GraphEntityKind::Vertex, Some("g"));
        selected.label = Some(expected);
        assert_eq!(a.graph_entity_ids(selected, None, 16).unwrap(), [1]);
        assert_eq!(a.graph_edge(10).unwrap().unwrap().source_id, 2);
        assert_eq!(b.graph_vertex(1).unwrap().unwrap().label, expected);
        assert_eq!(private.vertices[0].label, "beta");
    }
}

#[test]
fn native_graph_reads_reject_invalid_pages_filters_and_dangling_graph_state() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        with(&connection, |connection| {
            connection.execute_batch("INSERT INTO _graph_vertices VALUES (-1, 'node', '{}'); INSERT INTO _named_graphs VALUES ('dangling'), ('badkind'); INSERT INTO _graph_membership VALUES ('vertex', 99, 'dangling'), ('vertex', 1, 'unregistered'), ('unsupported', 1, 'badkind');")?;
            Ok(())
        });
        if native {
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
        }
        let catalog = Catalog::open(connection).unwrap();
        let vertices = filter(GraphEntityKind::Vertex, None);
        for limit in [0, 4097] {
            assert!(catalog.graph_entity_ids(vertices, None, limit).is_err());
        }
        assert!(catalog
            .graph_entity_ids(vertices, Some(u64::MAX), 1)
            .is_err());
        let mut invalid = vertices;
        invalid.source = Some(1);
        assert!(catalog.graph_entity_count(invalid).is_err());
        assert!(catalog.graph_entity_ids(invalid, None, 1).is_err());
        let mut invalid = filter(GraphEntityKind::Edge, Some("missing"));
        invalid.target = Some(u64::MAX);
        assert!(catalog.graph_entity_count(invalid).is_err());
        assert!(catalog.graph_entity_ids(invalid, None, 1).is_err());
        assert!(catalog.graph_vertex(u64::MAX).is_err());
        assert!(catalog.graph_edge(u64::MAX).is_err());
        assert_eq!(catalog.graph_entity_count(vertices).unwrap(), 3);
        assert_eq!(
            catalog
                .graph_entity_max_id(GraphEntityKind::Vertex)
                .unwrap(),
            Some(2)
        );
        assert!(catalog.graph_entity_ids(vertices, None, 16).is_err());
        assert!(catalog
            .graph_entity_ids(filter(GraphEntityKind::Vertex, Some("dangling")), None, 16)
            .is_err());
        assert!(catalog
            .graph_entity_count(filter(GraphEntityKind::Vertex, Some("dangling")))
            .is_err());
        for graph in ["dangling", "unregistered", "badkind"] {
            assert!(
                catalog.load_named_graph_snapshot(graph).is_err(),
                "{native} {graph}"
            );
        }
    }
}
