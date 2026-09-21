//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Semantic graph payload reads meet evaluated provider writes at the original entity identity.

use super::{finish, fixtures, Session};
use std::collections::BTreeMap;
use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{Direction, GraphStore};

#[path = "graph/topology.rs"]
mod topology;

#[path = "graph/paths.rs"]
mod paths;

#[path = "graph/definitions.rs"]
mod definitions;

#[path = "graph/labels.rs"]
mod labels;

fn prepare(session: &Session) {
    session.engine.create_graph("g").unwrap();
    for id in [1, 2] {
        session
            .engine
            .add_graph_vertex(Vertex::new(id, "P"), "g")
            .unwrap();
    }
    session
        .engine
        .add_graph_edge(Edge::new(10, 1, 2, "knows"), "g")
        .unwrap();
}

fn pivot(a: &Session, b: &Session) {
    b.sql("SELECT v FROM right_t");
    a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
}

fn replace_vertex(session: &Session, id: u64) {
    let mut vertex = Vertex::new(id, "P");
    vertex.properties.insert("value".into(), Value::Int(2));
    session.engine.add_graph_vertex(vertex, "g").unwrap();
}

#[test]
fn graph_payload_points_cover_absent_entities_and_preserve_unrelated_entities() {
    for (read_id, write_id, delete, conflict) in [
        (1, 1, false, true),
        (1, 2, false, false),
        (99, 99, false, true),
        (99, 2, false, false),
        (99, 99, true, false),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            let found = a
                .engine
                .graph_with("g", |store| store.get_vertex(read_id))
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(found.is_some(), read_id != 99);
            pivot(&a, &b);
            if delete {
                b.catalog.delete_vertex(write_id).unwrap();
            } else {
                replace_vertex(&b, write_id);
            }
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn graph_consumers_observe_returned_payloads_without_observing_internal_property_decodes() {
    for route in [
        "vertices",
        "label",
        "cypher",
        "edges",
        "edge",
        "ids",
        "neighbors",
        "edge ids",
        "counts",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            if route == "cypher" {
                let (_, rows) = a
                    .engine
                    .run_cypher("g", "MATCH (n:P) RETURN n", BTreeMap::default())
                    .unwrap();
                assert_eq!(rows.len(), 2);
            } else {
                a.engine
                    .graph_with("g", |store| {
                        match route {
                            "vertices" => {
                                assert_eq!(store.vertices_in_graph("g")?.len(), 2);
                            }
                            "label" => {
                                assert_eq!(store.vertices_by_label("P", "g")?.len(), 2);
                            }
                            "edges" => {
                                assert_eq!(store.edges_by_label("knows", "g")?.len(), 1);
                            }
                            "edge" => {
                                assert!(store.get_edge(10)?.is_some());
                            }
                            "ids" => {
                                assert_eq!(store.vertex_ids_by_label("P", "g")?.len(), 2);
                            }
                            "neighbors" => {
                                assert_eq!(
                                    store.neighbors(1, Some("knows"), Direction::Out, "g")?,
                                    vec![2]
                                );
                            }
                            "edge ids" => {
                                assert_eq!(store.out_edge_ids(1, "g")?.len(), 1);
                            }
                            "counts" => {
                                assert_eq!(store.vertex_label_counts("g")?.get("P"), Some(&2));
                            }
                            _ => unreachable!(),
                        }
                        uqa_graph::GraphStoreResult::Ok(())
                    })
                    .unwrap()
                    .unwrap()
                    .unwrap();
            }
            pivot(&a, &b);
            if matches!(route, "edges" | "edge" | "neighbors" | "edge ids") {
                let mut edge = Edge::new(10, 1, 2, "knows");
                edge.properties.insert("value".into(), Value::Int(2));
                b.engine.add_graph_edge(edge, "g").unwrap();
            } else {
                replace_vertex(&b, 1);
            }
            finish(
                &a,
                &b,
                matches!(route, "vertices" | "label" | "cypher" | "edges" | "edge"),
            );
        }
    }
}

#[test]
fn shared_graph_entities_observe_raw_catalog_replacements_and_deletions() {
    for delete in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            a.engine.create_graph("other").unwrap();
            a.engine
                .add_graph_vertex(Vertex::new(1, "P"), "other")
                .unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            a.engine
                .graph_with("other", |store| store.get_vertex(1))
                .unwrap()
                .unwrap()
                .unwrap();
            pivot(&a, &b);
            if delete {
                // Raw graph restore/cleanup paths must contribute the same entity write as graph callbacks.
                b.catalog.delete_vertex(1).unwrap();
            } else {
                b.catalog.save_vertex(1, "P", "{\"value\":2}").unwrap();
            }
            finish(&a, &b, true);
        }
    }
}

#[test]
fn graph_write_intents_follow_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        b.begin();
        a.sql("SELECT v FROM left_t");
        a.sql("SAVEPOINT before_graph");
        replace_vertex(&a, 1);
        a.sql("ROLLBACK TO SAVEPOINT before_graph");
        let vertex = b
            .engine
            .graph_with("g", |store| store.get_vertex(1))
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(vertex.properties.is_empty());
        b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
        finish(&a, &b, false);
    }
}

#[test]
fn retained_graph_readers_keep_the_original_participant_across_session_reuse() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        let retained = a.engine.graph_with("g", Clone::clone).unwrap().unwrap();
        assert!(retained
            .get_vertex(1)
            .unwrap()
            .unwrap()
            .properties
            .is_empty());
        b.begin();
        replace_vertex(&b, 1);
        b.engine.commit().unwrap();
        assert!(retained
            .get_vertex(1)
            .unwrap()
            .unwrap()
            .properties
            .is_empty());
        a.engine.rollback().unwrap();
        a.begin();
        assert!(retained.get_vertex(1).is_err());
        let current = a
            .engine
            .graph_with("g", |store| store.get_vertex(1))
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(current.properties.get("value"), Some(&Value::Int(2)));
        a.engine.commit().unwrap();
    }
}
