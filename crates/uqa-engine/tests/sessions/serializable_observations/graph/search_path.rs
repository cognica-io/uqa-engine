//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Effective search-path reads observe only consumed graph namespace decisions.

use super::{catalog_scalars::context, finish, fixtures, pivot};
use uqa_execution::catalog::cache::RegtypeOutputCache;

#[test]
fn search_path_queries_observe_graph_creation_and_removal_across_providers() {
    for (path, query, create, conflict) in [
        ("g, public", "SELECT current_schema()", false, true),
        ("missing, g, public", "SELECT current_schema()", true, true),
        ("g, missing, public", "SELECT current_schema()", true, false),
        ("public, g", "SELECT current_schema()", false, false),
        (
            "g, missing, public",
            "SELECT current_schemas(false)",
            true,
            true,
        ),
        ("public, g", "SELECT current_schemas(true)", false, true),
        (
            "missing, public",
            "SELECT current_schemas(false)",
            true,
            true,
        ),
        ("public", "SELECT current_schemas(true)", true, false),
        (
            "g, missing, public",
            "SELECT current_schema() LIMIT 0",
            false,
            false,
        ),
        (
            "g, missing, public",
            "SELECT current_schemas(false) LIMIT 0",
            true,
            false,
        ),
        (
            "g, missing, public",
            "EXPLAIN SELECT current_schema()",
            false,
            false,
        ),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.sql(&format!("SET search_path TO {path}"));
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql(query);
            pivot(&a, &b);
            if create {
                b.engine.create_graph("missing").unwrap();
            } else {
                b.engine.drop_graph("g").unwrap();
            }
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn direct_namespace_queries_share_transaction_observations_across_providers() {
    for list in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.sql("SET search_path TO g, public");
            let b = a.sibling();
            a.begin();
            b.begin();
            if list {
                assert_eq!(
                    a.engine.current_schema_names(false).unwrap(),
                    ["g", "public"]
                );
            } else {
                assert_eq!(a.engine.current_schema_name().unwrap(), Some("g".into()));
            }
            pivot(&a, &b);
            b.engine.drop_graph("g").unwrap();
            finish(&a, &b, true);
        }
    }
}

#[test]
fn retained_namespace_queries_keep_the_original_participant_across_refresh() {
    use uqa_execution::catalog::projection::{resolve_regobject_oid, resolve_regtype_output_value};
    use uqa_sql::ColumnType;
    let (_directory, sessions) = fixtures();
    for a in sessions {
        a.engine.create_graph("g").unwrap();
        a.sql("SET search_path TO g, public");
        a.begin();
        let cache = RegtypeOutputCache::default();
        context(&a, &cache)
            .with_query_reads(|catalog| {
                assert_eq!(catalog.current_schema_name()?, Some("g".into()));
                let original = catalog.catalog_read_view();
                a.engine.commit().unwrap();
                a.begin();
                assert_eq!(
                    resolve_regobject_oid(catalog, &ColumnType::Regtype, "integer")?,
                    Some(23)
                );
                let original_error = original.read_graph_names().unwrap_err();
                for error in [
                    catalog.current_schema_name().unwrap_err(),
                    catalog.current_schema_names(false).unwrap_err(),
                    resolve_regobject_oid(catalog, &ColumnType::Regproc, "random").unwrap_err(),
                    resolve_regobject_oid(catalog, &ColumnType::Regtype, "missing_type")
                        .unwrap_err(),
                    resolve_regtype_output_value(catalog, &ColumnType::Regproc, 1598).unwrap_err(),
                    resolve_regtype_output_value(catalog, &ColumnType::Regprocedure, 1598)
                        .unwrap_err(),
                ] {
                    assert_eq!(error.sqlstate(), original_error.sqlstate());
                    assert_eq!(error.to_string(), original_error.to_string());
                }
                a.engine.rollback().unwrap();
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn search_path_observations_survive_savepoint_rollback_and_do_not_observe_label_writes() {
    for label_only in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.sql("SET search_path TO g, public");
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql("SAVEPOINT read_namespace");
            a.sql("SELECT current_schemas(false)");
            a.sql("ROLLBACK TO SAVEPOINT read_namespace");
            pivot(&a, &b);
            if label_only {
                b.engine
                    .create_graph_label("g", "p", uqa_graph::LabelKind::Vertex)
                    .unwrap();
            } else {
                b.engine.drop_graph("g").unwrap();
            }
            finish(&a, &b, !label_only);
        }
    }
}
