//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Label metadata participates through semantic lookups without turning ID allocation into catalog-wide reads.

use super::{begin, finish, pivot, session, store, CancellationToken, GraphStore};
use uqa_graph::LabelKind;

#[test]
fn cypher_observes_only_required_default_relations_and_preserves_legacy_id_projection() {
    for (query, original, replacement, conflict) in [
        ("RETURN 1", "{}", r#"{"labels":{"P":3}}"#, false),
        ("MATCH (n) RETURN n", "{}", r#"{"labels":{"P":3}}"#, false),
        (
            "MATCH (n) RETURN n",
            "{}",
            r#"{"dropped_label_ids":[2]}"#,
            false,
        ),
        (
            "MATCH (n) RETURN n",
            "{}",
            r#"{"dropped_label_ids":[1]}"#,
            true,
        ),
        ("RETURN 1", "{}", r#"{"dropped_label_ids":[1,2]}"#, false),
        (
            "MATCH (a)-[r]->(b) RETURN r",
            "{}",
            r#"{"dropped_label_ids":[2]}"#,
            true,
        ),
        (
            "MATCH (n) RETURN n",
            r#"{"labels":{"legacy":1},"dropped_label_ids":[1]}"#,
            r#"{"dropped_label_ids":[1]}"#,
            true,
        ),
    ] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        store(&a).create_graph("g").unwrap();
        a.catalog
            .set_metadata("graph_label_registry::g", original)
            .unwrap();
        let reader = uqa_graph::GraphStoreHandle::Persistent(
            store(&a).with_serializable_read(begin(&a), &CancellationToken::new()),
        );
        let other = begin(&b);
        uqa_graph::cypher::validate_default_label_relations(
            &reader,
            "g",
            &uqa_graph::cypher::parse_cypher(query).unwrap(),
        )
        .unwrap();
        pivot(&a, &other);
        b.catalog
            .set_metadata("graph_label_registry::g", replacement)
            .unwrap();
        finish(&a, &b, conflict);
    }
}

#[test]
fn label_definitions_track_selected_names_defaults_and_catalog_lists() {
    for (read, write, conflict) in [
        ("point", "kind", true),
        ("point", "id", true),
        ("point", "remove", true),
        ("point", "other label", false),
        ("point", "other graph", false),
        ("point", "counters", false),
        ("point", "equivalent", false),
        ("point", "user tombstone", false),
        ("missing", "create", true),
        ("missing", "kind", false),
        ("default", "default", true),
        ("default", "kind", false),
        ("list", "create", true),
        ("list", "counters", false),
        ("registry", "counters", true),
        ("create existing", "remove", true),
        ("drop missing", "create", true),
        ("point", "delete metadata", true),
        ("point", "raw restore", true),
        ("point", "opaque", true),
    ] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        for graph in ["g", "other"] {
            store(&a).create_graph(graph).unwrap();
            a.catalog
                .set_metadata(
                    &format!("graph_label_registry::{graph}"),
                    r#"{"labels":{"P":3}}"#,
                )
                .unwrap();
        }
        let mut reader = store(&a).with_serializable_read(begin(&a), &CancellationToken::new());
        let other = begin(&b);
        match read {
            "point" => assert_eq!(
                reader.graph_label_kind("g", "P").unwrap(),
                Some(LabelKind::Vertex)
            ),
            "missing" => assert_eq!(reader.graph_label_kind("g", "new").unwrap(), None),
            "default" => assert_eq!(
                reader.graph_label_kind("g", "_ag_label_vertex").unwrap(),
                Some(LabelKind::Vertex)
            ),
            "list" => assert_eq!(reader.graph_labels("g").unwrap().len(), 3),
            "registry" => assert_eq!(reader.label_registry("g").unwrap().labels["P"], 3),
            "create existing" => assert_eq!(
                reader.create_label("g", "P", LabelKind::Vertex).unwrap(),
                None
            ),
            "drop missing" => assert_eq!(reader.drop_label("g", "new").unwrap(), None),
            _ => unreachable!(),
        }
        pivot(&a, &other);
        let json = match write {
            "kind" | "raw restore" | "other graph" => r#"{"labels":{"P":3},"kinds":{"P":"e"}}"#,
            "id" => r#"{"labels":{"P":4}}"#,
            "remove" => "{}",
            "other label" => r#"{"labels":{"P":3,"Q":4}}"#,
            "create" => r#"{"labels":{"P":3,"new":4}}"#,
            "counters" => r#"{"labels":{"P":3},"next_label_id":10,"sequences":{"3":70}}"#,
            "equivalent" => r#"{"kinds":{"P":"v"},"labels":{"\u0050":3}}"#,
            "default" => r#"{"labels":{"P":3},"dropped_label_ids":[1]}"#,
            "user tombstone" => r#"{"labels":{"P":3},"dropped_label_ids":[3]}"#,
            "opaque" => "legacy opaque registry",
            "delete metadata" => "",
            _ => unreachable!(),
        };
        match write {
            "delete metadata" => b
                .catalog
                .delete_metadata("graph_label_registry::g")
                .unwrap(),
            "raw restore" => {
                let mut snapshot = b.catalog.load_named_graph_snapshot("g").unwrap().unwrap();
                snapshot.label_registry_json = json.into();
                b.catalog.replace_named_graph("g", &snapshot).unwrap();
            }
            _ => b
                .catalog
                .set_metadata(
                    if write == "other graph" {
                        "graph_label_registry::other"
                    } else {
                        "graph_label_registry::g"
                    },
                    json,
                )
                .unwrap(),
        }
        finish(&a, &b, conflict);
    }
}

#[test]
fn rolled_back_label_definitions_remove_intents_and_keep_prior_dependencies() {
    for earlier in [false, true] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        store(&a).create_graph("g").unwrap();
        let reader = store(&a).with_serializable_read(begin(&a), &CancellationToken::new());
        let other = begin(&b);
        if earlier {
            assert!(reader.graph_label_kind("g", "new").unwrap().is_none());
        }
        pivot(&a, &other);
        let mark = uqa_storage::StorageSavepointId::allocate();
        b.backend.savepoint(mark).unwrap();
        store(&b)
            .create_label("g", "new", LabelKind::Vertex)
            .unwrap();
        b.backend.rollback_to_savepoint(mark).unwrap();
        b.backend.release_savepoint(mark).unwrap();
        assert!(store(&b).graph_label_kind("g", "new").unwrap().is_none());
        if !earlier {
            assert!(reader.graph_label_kind("g", "new").unwrap().is_none());
        }
        b.catalog.set_metadata("unrelated", "1").unwrap();
        finish(&a, &b, earlier);
    }
}

#[test]
fn retained_label_readers_keep_original_attribution_and_cancellation() {
    for cancelled in [false, true] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        store(&a).create_graph("g").unwrap();
        let context = begin(&a);
        let other = begin(&b);
        let cancellation = CancellationToken::new();
        let retained = a.backend.open_retained_read_session(&cancellation).unwrap();
        let reader = store(&retained).with_serializable_read(context, &cancellation);
        if cancelled {
            cancellation.cancel();
        }
        let result = reader.graph_label_kind("g", "new");
        if cancelled {
            assert!(super::has_cause(&result.unwrap_err(), |error| matches!(
                error.downcast_ref::<uqa_storage::StorageBackendError>(),
                Some(uqa_storage::StorageBackendError::Cancelled(_))
            )));
            a.backend.rollback_transaction().unwrap();
            b.backend.rollback_transaction().unwrap();
        } else {
            assert!(result.unwrap().is_none());
            pivot(&a, &other);
            store(&b)
                .create_label("g", "new", LabelKind::Vertex)
                .unwrap();
            finish(&a, &b, true);
        }
    }
}
