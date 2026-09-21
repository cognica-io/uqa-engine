//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cached graph catalog reads and provider definition mutations share the original transaction.

use super::{finish, fixtures, pivot};

#[test]
fn graph_definition_statistics_do_not_manufacture_dependencies() {
    use uqa_planner::retrieval_planning::RetrievalPlanningCatalog;
    let (_directory, sessions) = fixtures();
    for a in sessions {
        a.engine.create_graph("g").unwrap();
        let b = a.sibling();
        a.begin();
        b.begin();
        assert!(a.engine.graph_snapshot("g").unwrap().is_some());
        assert!(a.engine.graph_snapshot("missing").unwrap().is_none());
        pivot(&a, &b);
        b.catalog.drop_named_graph_data("g").unwrap();
        b.catalog.save_named_graph("missing").unwrap();
        finish(&a, &b, false);
    }
}

#[test]
fn cached_graph_names_observe_present_absent_and_listing_changes_across_the_three_providers() {
    for (read, write, conflict) in [
        ("missing", "create", true),
        ("missing", "other", false),
        ("missing", "drop missing", false),
        ("present", "drop", true),
        ("present", "same", false),
        ("list", "create", true),
        ("handle", "create", true),
        ("labels", "create", true),
        ("catalog", "create", true),
        ("create existing", "drop", true),
        ("drop missing", "create", true),
        ("mutate missing", "create", true),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            match read {
                "missing" => assert!(!a.engine.has_graph("new").unwrap()),
                "present" => assert!(a.engine.has_graph("g").unwrap()),
                "list" => assert_eq!(a.engine.list_graphs().unwrap(), ["g"]),
                "handle" => assert!(a.engine.graph_with("new", |_| ()).unwrap().is_none()),
                "labels" => assert!(a.engine.list_graph_labels("new").unwrap().is_none()),
                "catalog" => assert_eq!(a.engine.graph_label_catalog().unwrap().len(), 1),
                "create existing" => assert!(!a.engine.create_graph("g").unwrap()),
                "drop missing" => assert!(!a.engine.drop_graph("new").unwrap()),
                "mutate missing" => assert!(a
                    .engine
                    .graph_with_mut("new", |_| Ok(()))
                    .unwrap()
                    .is_none()),
                _ => unreachable!(),
            }
            pivot(&a, &b);
            match write {
                "create" => b.catalog.save_named_graph("new").unwrap(),
                "other" => b.catalog.save_named_graph("other").unwrap(),
                "drop missing" => b.catalog.drop_named_graph("new").unwrap(),
                "drop" => b.catalog.drop_named_graph_data("g").unwrap(),
                "same" => b.catalog.save_named_graph("g").unwrap(),
                _ => unreachable!(),
            }
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn cached_path_definitions_observe_replacement_deletion_and_absence_across_the_three_providers() {
    for (read, write, conflict) in [
        ("present", "replace", true),
        ("present", "drop", true),
        ("present", "same", false),
        ("present", "other", false),
        ("present", "drop graph", true),
        ("missing", "create", true),
        ("missing", "drop missing", false),
        ("list", "create", true),
        ("drop missing", "create", true),
        ("missing graph", "create graph", true),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.engine
                .build_path_index("p", "g", &[vec!["knows".into()]])
                .unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            match read {
                "present" => assert!(a.engine.get_path_index("p", "g").unwrap().is_some()),
                "missing" => assert!(a.engine.get_path_index("new", "g").unwrap().is_none()),
                "list" => assert_eq!(a.engine.list_path_indexes().unwrap(), ["g::p"]),
                "drop missing" => assert!(!a.engine.drop_path_index("new", "g").unwrap()),
                "missing graph" => assert!(!a.engine.build_path_index("p", "new", &[]).unwrap()),
                _ => unreachable!(),
            }
            pivot(&a, &b);
            match write {
                "replace" => b.catalog.save_path_index("g::p", "[]").unwrap(),
                "drop" => b.catalog.drop_path_index("g::p").unwrap(),
                "same" => b.catalog.save_path_index("g::p", "[[\"knows\"]]").unwrap(),
                "other" => b.catalog.save_path_index("g::other", "[]").unwrap(),
                "create" => b.catalog.save_path_index("g::new", "[]").unwrap(),
                "drop missing" => b.catalog.drop_path_index("g::new").unwrap(),
                "drop graph" => b.catalog.drop_named_graph_data("g").unwrap(),
                "create graph" => b.catalog.save_named_graph("new").unwrap(),
                _ => unreachable!(),
            }
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn graph_definition_undo_removes_intents_and_retains_observed_dependencies_across_the_three_providers(
) {
    for read_before_rollback in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let b = a.sibling();
            a.begin();
            b.begin();
            if read_before_rollback {
                assert!(!a.engine.has_graph("new").unwrap());
                assert!(a.engine.list_path_indexes().unwrap().is_empty());
            }
            pivot(&a, &b);
            b.sql("SAVEPOINT undo_definitions");
            b.catalog.save_named_graph("new").unwrap();
            b.catalog.save_path_index("new::p", "[]").unwrap();
            b.sql("ROLLBACK TO SAVEPOINT undo_definitions");
            assert!(b.catalog.load_named_graphs().unwrap().is_empty());
            assert!(b.catalog.load_path_indexes().unwrap().is_empty());
            if !read_before_rollback {
                assert!(!a.engine.has_graph("new").unwrap());
                assert!(a.engine.list_path_indexes().unwrap().is_empty());
            }
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            finish(&a, &b, read_before_rollback);
        }
    }
}
