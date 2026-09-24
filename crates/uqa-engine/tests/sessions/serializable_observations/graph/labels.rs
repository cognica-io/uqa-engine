//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine caches bind label observations to their original provider transaction.

use super::{finish, fixtures, pivot};
use uqa_graph::{GraphLabelRegistry, LabelKind};

fn replace_registry(b: &super::Session, write: &str) {
    let mut registry: GraphLabelRegistry = serde_json::from_str(
        &b.catalog
            .get_metadata("graph_label_registry::g")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    match write {
        "kind" | "restore" => {
            registry.kinds.insert("P".into(), LabelKind::Edge);
        }
        "other" | "create" => {
            registry
                .labels
                .insert(if write == "other" { "Q" } else { "new" }.into(), 17);
        }
        "counters" => {
            registry.next_label_id += 20;
            registry.sequences.insert(registry.labels["P"], 70);
        }
        "remove" => {
            registry.remove_label("P");
        }
        "default" => {
            registry.dropped_label_ids.insert(1);
        }
        "delete" => {}
        _ => unreachable!(),
    }
    let json = serde_json::to_string(&registry).unwrap();
    match write {
        "delete" => b
            .catalog
            .delete_metadata("graph_label_registry::g")
            .unwrap(),
        "restore" => {
            let mut snapshot = b.catalog.load_named_graph_snapshot("g").unwrap().unwrap();
            snapshot.label_registry_json = json;
            b.catalog.replace_named_graph("g", &snapshot).unwrap();
        }
        _ => b
            .catalog
            .set_metadata("graph_label_registry::g", &json)
            .unwrap(),
    }
}

#[test]
fn cypher_default_label_checks_do_not_observe_unrelated_definitions_across_the_three_providers() {
    for (query, changed, conflict) in [
        ("RETURN 1", 1, false),
        ("MATCH (n) RETURN n", 1, true),
        ("MATCH (n) RETURN n", 2, false),
        ("MATCH (n) RETURN n", 3, false),
        ("MATCH (a)-[r]->(b) RETURN r", 2, true),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            a.engine
                .run_cypher("g", query, std::collections::BTreeMap::new())
                .unwrap();
            pivot(&a, &b);
            let json = if changed == 3 {
                r#"{"labels":{"new":3}}"#.to_owned()
            } else {
                format!("{{\"dropped_label_ids\":[{changed}]}}")
            };
            b.catalog
                .set_metadata("graph_label_registry::g", &json)
                .unwrap();
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn graph_label_definitions_preserve_points_lists_and_no_op_decisions_across_the_three_providers() {
    for (read, write, conflict) in [
        ("point", "kind", true),
        ("point", "other", false),
        ("point", "counters", false),
        ("point", "remove", true),
        ("absent", "create", true),
        ("absent", "kind", false),
        ("list", "create", true),
        ("list", "counters", false),
        ("catalog", "create", true),
        ("catalog", "counters", false),
        ("registry", "counters", true),
        ("default", "default", true),
        ("create existing", "remove", true),
        ("drop missing", "create", true),
        ("point", "delete", true),
        ("point", "restore", true),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.engine
                .create_graph_label("g", "P", LabelKind::Vertex)
                .unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            match read {
                "point" | "absent" | "default" => {
                    let label = match read {
                        "point" => "P",
                        "default" => "_ag_label_vertex",
                        _ => "new",
                    };
                    let kind = a
                        .engine
                        .graph_with("g", |store| store.graph_label_kind("g", label))
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    assert_eq!(kind, (read != "absent").then_some(LabelKind::Vertex));
                }
                "list" => assert_eq!(a.engine.list_graph_labels("g").unwrap().unwrap().len(), 3),
                "catalog" => assert_eq!(a.engine.graph_label_catalog().unwrap()[0].1.len(), 3),
                "registry" => assert_eq!(
                    a.engine
                        .graph_with("g", |store| store.label_registry("g"))
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .labels
                        .len(),
                    1
                ),
                "create existing" => assert!(!a
                    .engine
                    .create_graph_label("g", "P", LabelKind::Vertex)
                    .unwrap()),
                "drop missing" => assert!(!a.engine.drop_graph_label("g", "new").unwrap()),
                _ => unreachable!(),
            }
            pivot(&a, &b);
            replace_registry(&b, write);
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn graph_label_savepoints_remove_cancelled_intents_across_the_three_providers() {
    for earlier in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            if earlier {
                assert_eq!(a.engine.list_graph_labels("g").unwrap().unwrap().len(), 2);
            }
            pivot(&a, &b);
            b.sql("SAVEPOINT undo_label");
            b.engine
                .create_graph_label("g", "new", LabelKind::Vertex)
                .unwrap();
            b.sql("ROLLBACK TO SAVEPOINT undo_label");
            assert_eq!(b.engine.list_graph_labels("g").unwrap().unwrap().len(), 2);
            if !earlier {
                assert_eq!(a.engine.list_graph_labels("g").unwrap().unwrap().len(), 2);
            }
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            finish(&a, &b, earlier);
        }
    }
}
