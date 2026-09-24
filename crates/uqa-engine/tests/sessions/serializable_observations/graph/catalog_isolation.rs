//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` system metadata is outside SSI; ordinary and AGE data still retain predicates.

use super::{finish, fixtures, pivot};
use uqa_core::Value;
use uqa_execution::catalog::{
    cache::RegtypeOutputCache,
    context::CatalogContext,
    projection::{build_info_schema_rows, resolve_regtype_output},
    services::{CatalogSession, CatalogSnapshotSource},
};

#[test]
fn system_catalog_and_namespace_queries_do_not_conflict_with_graph_definitions() {
    // The nine forms match the pinned PostgreSQL 18.4 metadata schedules in the implementation plan.
    for query in [
        "SELECT count(*) AS value FROM pg_catalog.pg_namespace",
        "SELECT count(*) AS value FROM pg_catalog.pg_class",
        "SELECT count(*) AS value FROM information_schema.schemata",
        "SELECT 'g'::regnamespace::oid AS value",
        "SELECT 'g'::regnamespace::text AS value",
        "SELECT to_regnamespace('missing') AS value",
        "SELECT current_schema() AS value",
        "SELECT current_schemas(false) AS value",
        "SELECT has_schema_privilege('g', 'USAGE') AS value",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.sql("SET search_path TO g, public");
            let b = a.sibling();
            a.begin();
            b.begin();
            let result = a.sql(query);
            assert_eq!(result.rows.len(), 1, "{query}");
            assert_eq!(
                result.rows[0]["value"] == Value::Null,
                query.contains("missing")
            );
            pivot(&a, &b);
            if query.contains("missing") {
                b.engine.create_graph("missing").unwrap();
            } else {
                b.engine.drop_graph("g").unwrap();
            }
            finish(&a, &b, false);
        }
    }
}

#[test]
fn namespace_privilege_overloads_do_not_create_graph_predicates() {
    for expression in [
        "has_schema_privilege('g', 'USAGE')",
        "has_schema_privilege('uqa', 'g', 'USAGE')",
        "has_schema_privilege(@role::oid, 'g', 'USAGE')",
        "has_schema_privilege(@graph::oid, 'USAGE')",
        "has_schema_privilege('uqa', @graph::oid, 'USAGE')",
        "has_schema_privilege(@role::oid, @graph::oid, 'USAGE')",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            let oids = a
                .sql("SELECT 'g'::regnamespace::oid AS graph_oid, 'uqa'::regrole::oid AS role_oid");
            let Value::Int(graph_oid) = oids.rows[0]["graph_oid"] else {
                panic!("graph OID")
            };
            let Value::Int(role_oid) = oids.rows[0]["role_oid"] else {
                panic!("role OID")
            };
            let expression = expression
                .replace("@graph", &graph_oid.to_string())
                .replace("@role", &role_oid.to_string());
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(
                a.sql(&format!("SELECT {expression} AS allowed")).rows[0]["allowed"],
                Value::Bool(true)
            );
            pivot(&a, &b);
            b.engine.drop_graph("g").unwrap();
            finish(&a, &b, false);
        }
    }
}

#[test]
fn cached_and_savepoint_namespace_inquiries_remain_metadata_reads() {
    for warm in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.engine.create_graph("g").unwrap();
            a.sql("SET search_path TO g, public");
            let b = a.sibling();
            a.begin();
            b.begin();
            if warm {
                a.sql("SELECT to_regtype('integer')::oid");
            }
            a.sql("SAVEPOINT metadata");
            assert_eq!(
                a.sql("SELECT 'g'::regnamespace::text AS name").rows[0]["name"],
                Value::Str("g".into())
            );
            assert_eq!(a.engine.current_schema_name().unwrap(), Some("g".into()));
            assert_eq!(
                a.engine.current_schema_names(false).unwrap(),
                ["g", "public"]
            );
            a.sql("ROLLBACK TO SAVEPOINT metadata");
            pivot(&a, &b);
            b.engine.drop_graph("g").unwrap();
            finish(&a, &b, false);
        }
    }
}

#[test]
fn explicit_catalog_projections_exempt_metadata_without_disabling_original_data_reads() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        a.engine.create_graph("g").unwrap();
        a.begin();
        let catalog = a
            .engine
            .bind_query_reads(a.engine.catalog_snapshot())
            .unwrap();
        a.engine.commit().unwrap();
        a.begin();
        let cache = RegtypeOutputCache::default();
        let context = CatalogContext {
            catalog: &catalog,
            session: &a.engine,
            namespaces: &a.engine,
            routines: &a.engine,
            expressions: &a.engine,
            counts: &a.engine,
            views: &a.engine,
            cache: &cache,
        };
        let resolution = a.engine.relation_name_resolution();
        for name in [
            "pg_catalog.pg_namespace",
            "pg_catalog.pg_class",
            "pg_catalog.pg_attribute",
            "information_schema.schemata",
        ] {
            assert!(
                !build_info_schema_rows(&context, &catalog, &resolution, &a.engine, name)
                    .unwrap()
                    .unwrap()
                    .is_empty(),
                "{name}"
            );
        }
        for _ in 0..2 {
            assert_eq!(
                resolve_regtype_output(&context, &uqa_sql::ColumnType::Regtype, 23).unwrap(),
                Some("integer".into())
            );
        }
        for name in ["ag_catalog.ag_graph", "ag_catalog.ag_label"] {
            assert!(
                build_info_schema_rows(&context, &catalog, &resolution, &a.engine, name).is_err(),
                "{name}"
            );
        }
        a.engine.rollback().unwrap();
    }
}

#[test]
fn ordinary_tables_named_like_system_catalogs_still_detect_serialization_cycles() {
    for reference in ["public.pg_namespace", "pg_namespace"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.sql("CREATE TABLE public.pg_namespace (id INT PRIMARY KEY, v INT)");
            a.sql("INSERT INTO public.pg_namespace VALUES (1, 1)");
            a.sql("SET search_path TO public, pg_catalog");
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(a.sql(&format!("SELECT v FROM {reference}")).rows.len(), 1);
            pivot(&a, &b);
            b.sql("UPDATE public.pg_namespace SET v = 2 WHERE id = 1");
            finish(&a, &b, true);
        }
    }
}
