//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL graph catalogs observe consumed rows under the original provider transaction.

use super::{finish, fixtures, pivot};
use uqa_core::Vertex;
use uqa_graph::LabelKind;

#[test]
fn sql_graph_catalog_reads_track_definitions_only_when_consumed_across_the_three_providers() {
    for (query, write, conflict) in [
        ("SELECT count(*) FROM ag_catalog.ag_label", "label", true),
        (
            "SELECT name FROM ag_catalog.ag_label LIMIT 0",
            "label",
            false,
        ),
        (
            "EXPLAIN SELECT name FROM ag_catalog.ag_label",
            "label",
            false,
        ),
        ("SELECT count(*) FROM ag_catalog.ag_graph", "create", true),
        ("SELECT count(*) FROM ag_catalog.ag_graph", "drop", true),
        ("SELECT count(*) FROM ag_catalog.ag_graph", "label", false),
        (
            "SELECT name FROM ag_catalog.ag_graph LIMIT 0",
            "create",
            false,
        ),
        (
            "SELECT count(*) FROM pg_catalog.pg_namespace",
            "create",
            true,
        ),
        (
            "SELECT count(*) FROM pg_catalog.pg_namespace",
            "label",
            false,
        ),
        (
            "SELECT count(*) FROM information_schema.schemata",
            "create",
            true,
        ),
        ("SELECT id FROM g._ag_label_vertex", "vertex", true),
        ("SELECT id FROM g._ag_label_vertex LIMIT 0", "vertex", false),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql(query);
            pivot(&a, &b);
            match write {
                "label" => {
                    b.engine
                        .create_graph_label("g", "new", LabelKind::Vertex)
                        .unwrap();
                }
                "create" => {
                    b.engine.create_graph("new").unwrap();
                }
                "drop" => {
                    b.engine.drop_graph("g").unwrap();
                }
                "vertex" => b.engine.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap(),
                _ => unreachable!(),
            }
            finish(&a, &b, conflict);
            if conflict {
                assert_eq!(b.engine.list_graphs().unwrap(), ["g"]);
                b.sql("SELECT count(*) FROM ag_catalog.ag_label");
            }
        }
    }
}

#[test]
fn retained_sql_graph_catalogs_keep_the_original_participant_across_the_three_providers() {
    use uqa_execution::catalog::services::CatalogSnapshotSource;
    let (_directory, sessions) = fixtures();
    for a in sessions {
        a.engine.create_graph("g").unwrap();
        a.begin();
        let catalog = a
            .engine
            .bind_query_reads(a.engine.catalog_snapshot())
            .unwrap();
        assert_eq!(catalog.read_graph_names().unwrap(), ["g"]);
        a.engine.commit().unwrap();
        a.begin();
        assert!(catalog.read_graph_names().is_err());
        a.engine.rollback().unwrap();
    }
}

#[test]
fn empty_sql_graph_catalogs_observe_concurrent_creation_across_the_three_providers() {
    for catalog in ["ag_graph", "ag_label"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql(&format!("SELECT count(*) FROM ag_catalog.{catalog}"));
            pivot(&a, &b);
            b.engine.create_graph("new").unwrap();
            finish(&a, &b, true);
            assert!(b.engine.list_graphs().unwrap().is_empty());
        }
    }
}

#[test]
fn graph_catalog_restoration_uses_the_rolled_back_session_overlay_across_the_three_providers() {
    for boundary in ["transaction", "savepoint", "statement"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.begin();
            if boundary == "savepoint" {
                a.sql("SAVEPOINT graph_names");
            }
            a.engine.create_graph("discarded").unwrap();
            match boundary {
                "transaction" => {
                    a.engine.rollback().unwrap();
                }
                "savepoint" => {
                    a.sql("ROLLBACK TO SAVEPOINT graph_names");
                    assert_eq!(a.engine.list_graphs().unwrap(), ["g"]);
                    a.engine.commit().unwrap();
                }
                "statement" => {
                    let error = a
                        .engine
                        .sql("INSERT INTO left_t VALUES (1, 99)", &[])
                        .unwrap_err();
                    assert_eq!(error.sqlstate(), Some("23505"), "{error}");
                    a.sql("ROLLBACK");
                }
                _ => unreachable!(),
            }
            assert_eq!(a.engine.list_graphs().unwrap(), ["g"]);
            a.sql("SELECT count(*) FROM ag_catalog.ag_label");
        }
    }
}

#[test]
fn sql_graph_catalog_savepoint_undo_preserves_consumed_dependencies_across_the_three_providers() {
    for read_before_rollback in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            if read_before_rollback {
                a.sql("SELECT count(*) FROM ag_catalog.ag_label");
            }
            pivot(&a, &b);
            b.sql("SAVEPOINT labels");
            b.engine
                .create_graph_label("g", "new", LabelKind::Vertex)
                .unwrap();
            b.sql("ROLLBACK TO SAVEPOINT labels");
            if !read_before_rollback {
                a.sql("SELECT count(*) FROM ag_catalog.ag_label");
            }
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            finish(&a, &b, read_before_rollback);
        }
    }
}
