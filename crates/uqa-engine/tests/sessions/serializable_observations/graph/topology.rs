//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session adapters preserve semantic graph phantoms across native `SQLite`, Key/Value `SQLite` and redb.

use super::{finish, fixtures, pivot, prepare, Direction, Edge, GraphStore, Session, Vertex};
use std::collections::BTreeMap;
use uqa_graph::{GraphStoreHandle, GraphStoreResult};

fn read(session: &Session, operation: impl FnOnce(&GraphStoreHandle) -> GraphStoreResult<()>) {
    session
        .engine
        .graph_with("g", |store| operation(store))
        .unwrap()
        .unwrap()
        .unwrap();
}

fn other_graph(session: &Session) {
    session.engine.create_graph("other").unwrap();
    for id in [1, 2] {
        session
            .engine
            .add_graph_vertex(Vertex::new(id, "P"), "other")
            .unwrap();
    }
}

#[test]
fn empty_graph_label_results_observe_matching_insertions_without_crossing_other_labels_or_graphs() {
    for cypher in [false, true] {
        for (graph, label, conflict) in [("g", "Q", true), ("g", "R", false), ("other", "Q", false)]
        {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                other_graph(&a);
                a.engine
                    .create_graph_label("g", "Q", uqa_graph::LabelKind::Vertex)
                    .unwrap();
                let b = a.sibling();
                a.begin();
                b.begin();
                if cypher {
                    let (_, rows) = a
                        .engine
                        .run_cypher("g", "MATCH (n:Q) RETURN n", BTreeMap::default())
                        .unwrap();
                    assert!(rows.is_empty());
                } else {
                    read(&a, |store| {
                        assert!(store.vertex_ids_by_label("Q", "g")?.is_empty());
                        Ok(())
                    });
                }
                pivot(&a, &b);
                b.engine
                    .add_graph_vertex(Vertex::new(3, label), graph)
                    .unwrap();
                finish(&a, &b, conflict);
            }
        }
    }
}

#[test]
fn empty_graph_adjacency_results_keep_graph_label_and_endpoint_filters() {
    for (graph, label, source, target, conflict) in [
        ("g", "likes", 1, 2, true),
        ("g", "likes", 2, 1, false),
        ("g", "knows", 1, 2, false),
        ("other", "likes", 1, 2, false),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            other_graph(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            read(&a, |store| {
                assert!(store
                    .neighbors(1, Some("likes"), Direction::Out, "g")?
                    .is_empty());
                Ok(())
            });
            pivot(&a, &b);
            b.engine
                .add_graph_edge(Edge::new(11, source, target, label), graph)
                .unwrap();
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn graph_page_suffixes_and_membership_reads_detect_phantoms_at_the_original_identity() {
    for route in [
        "page",
        "earlier id",
        "membership",
        "other entity",
        "counts",
        "endpoint",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            read(&a, |store| {
                match route {
                    "page" | "earlier id" => {
                        assert!(store.vertex_id_page("g", Some(2), 8)?.is_empty());
                    }
                    "membership" | "other entity" => assert!(store.vertex_graphs(99)?.is_empty()),
                    "counts" => assert_eq!(store.vertex_label_counts("g")?.get("P"), Some(&2)),
                    "endpoint" => {
                        assert_eq!(store.neighbors(1, None, Direction::Out, "g")?, vec![2]);
                    }
                    _ => unreachable!(),
                }
                Ok(())
            });
            pivot(&a, &b);
            match route {
                "page" => b.engine.add_graph_vertex(Vertex::new(3, "P"), "g").unwrap(),
                "earlier id" | "counts" => {
                    b.engine.add_graph_vertex(Vertex::new(2, "Q"), "g").unwrap();
                }
                "membership" => b
                    .engine
                    .add_graph_vertex(Vertex::new(99, "P"), "g")
                    .unwrap(),
                "other entity" => b
                    .engine
                    .add_graph_vertex(Vertex::new(98, "P"), "g")
                    .unwrap(),
                "endpoint" => b
                    .engine
                    .add_graph_edge(Edge::new(10, 1, 1, "knows"), "g")
                    .unwrap(),
                _ => unreachable!(),
            }
            finish(&a, &b, !matches!(route, "earlier id" | "other entity"));
        }
    }
}

#[test]
fn raw_graph_replacement_observes_new_members_using_the_evaluated_entity_fields() {
    for edge in [false, true] {
        for existing_entity in [false, true] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                if existing_entity {
                    other_graph(&a);
                    if edge {
                        a.engine
                            .add_graph_edge(Edge::new(11, 1, 2, "R"), "other")
                            .unwrap();
                    } else {
                        a.engine
                            .add_graph_vertex(Vertex::new(3, "R"), "other")
                            .unwrap();
                    }
                }
                let mut replacement = a.catalog.load_named_graph_snapshot("g").unwrap().unwrap();
                let mut registry: uqa_graph::GraphLabelRegistry =
                    serde_json::from_str(&replacement.label_registry_json).unwrap();
                registry
                    .register_label(
                        "Q",
                        if edge {
                            uqa_graph::LabelKind::Edge
                        } else {
                            uqa_graph::LabelKind::Vertex
                        },
                    )
                    .unwrap();
                replacement.label_registry_json = serde_json::to_string(&registry).unwrap();
                if edge {
                    replacement.edges.push(uqa_storage::EdgeRow {
                        edge_id: 11,
                        source_id: 1,
                        target_id: 2,
                        label: "Q".into(),
                        properties_json: "{}".into(),
                    });
                } else {
                    replacement.vertices.push(uqa_storage::GraphVertexRow {
                        vertex_id: 3,
                        label: "Q".into(),
                        properties_json: "{}".into(),
                    });
                }
                let b = a.sibling();
                a.begin();
                b.begin();
                read(&a, |store| {
                    if edge {
                        assert!(store
                            .neighbors(1, Some("Q"), Direction::Out, "g")?
                            .is_empty());
                    } else {
                        assert!(store.vertex_ids_by_label("Q", "g")?.is_empty());
                    }
                    Ok(())
                });
                pivot(&a, &b);
                b.catalog.replace_named_graph("g", &replacement).unwrap();
                finish(&a, &b, true);
            }
        }
    }
}

#[test]
fn graph_clear_reaches_absent_reads_from_the_prior_identifier_generation() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        b.begin();
        read(&a, |store| {
            assert!(store.vertex_ids_by_label("Q", "g")?.is_empty());
            Ok(())
        });
        pivot(&a, &b);
        b.engine
            .graph_with_mut("g", |store| {
                store.clear()?;
                store.create_graph("g")?;
                store.add_vertex(Vertex::new(99, "Q"), "g")
            })
            .unwrap()
            .unwrap();
        finish(&a, &b, true);
    }
}

#[test]
fn graph_selector_write_intents_disappear_with_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        b.begin();
        a.sql("SELECT v FROM left_t");
        a.sql("SAVEPOINT graph_changes");
        a.engine
            .add_graph_vertex(Vertex::new(99, "Q"), "g")
            .unwrap();
        a.sql("ROLLBACK TO graph_changes");
        read(&b, |store| {
            assert!(store.vertex_ids_by_label("Q", "g")?.is_empty());
            Ok(())
        });
        b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
        finish(&a, &b, false);
    }
}

#[test]
fn planner_graph_statistics_do_not_manufacture_payload_or_topology_dependencies() {
    use uqa_planner::retrieval_planning::RetrievalPlanningCatalog;
    for insert in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            let statistics = a.engine.graph_snapshot("g").unwrap().unwrap();
            assert_eq!(statistics.vertices.len(), 2);
            assert_eq!(statistics.edges.len(), 1);
            pivot(&a, &b);
            if insert {
                b.engine.add_graph_vertex(Vertex::new(3, "P"), "g").unwrap();
            } else {
                super::replace_vertex(&b, 1);
            }
            finish(&a, &b, false);
        }
    }
}
