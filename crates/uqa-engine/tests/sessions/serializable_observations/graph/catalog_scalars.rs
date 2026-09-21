//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph namespace scalar reads retain participants independently from shared catalog caches.

use super::{finish, fixtures, pivot, Session};
use uqa_core::{Value, Vertex};
use uqa_execution::catalog::{cache::RegtypeOutputCache, context::CatalogContext};
use uqa_graph::LabelKind;

fn prepare(a: &Session) {
    a.engine.create_graph("g").unwrap();
    a.engine
        .create_graph_label("g", "p", LabelKind::Vertex)
        .unwrap();
}

fn write(b: &Session, operation: &str) {
    match operation {
        "drop graph" => {
            b.engine.drop_graph("g").unwrap();
        }
        "create graph" => {
            b.engine.create_graph("missing").unwrap();
        }
        "label" => {
            b.engine
                .create_graph_label("g", "new", LabelKind::Vertex)
                .unwrap();
        }
        "vertex" => {
            b.engine.add_graph_vertex(Vertex::new(1, "p"), "g").unwrap();
        }
        _ => unreachable!(),
    }
}

pub(super) fn context<'a>(a: &'a Session, cache: &'a RegtypeOutputCache) -> CatalogContext<'a> {
    CatalogContext {
        catalog: &a.engine,
        session: &a.engine,
        namespaces: &a.engine,
        routines: &a.engine,
        expressions: &a.engine,
        counts: &a.engine,
        views: &a.engine,
        cache,
    }
}

#[test]
fn graph_namespace_scalar_names_observe_present_and_absent_definitions_across_providers() {
    for warm in [false, true] {
        for (expression, write_kind, conflict) in [
            ("'g'::regnamespace::oid", "drop graph", true),
            ("to_regnamespace('g')::oid", "drop graph", true),
            ("to_regnamespace('missing')::oid", "create graph", true),
            ("to_regnamespace('g')::oid", "label", false),
            ("to_regnamespace('g')::oid", "create graph", false),
        ] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                let b = a.sibling();
                a.begin();
                b.begin();
                if warm {
                    a.sql("SELECT to_regtype('integer')::oid");
                }
                let result = a.sql(&format!("SELECT {expression} AS value"));
                assert_eq!(
                    result.rows[0]["value"] == Value::Null,
                    expression.contains("missing")
                );
                pivot(&a, &b);
                write(&b, write_kind);
                finish(&a, &b, conflict);
            }
        }
    }
}

#[test]
fn graph_namespace_scalar_output_observes_cached_identities_across_providers() {
    for warm in [false, true] {
        for (present, write_kind, conflict) in [
            (true, "drop graph", true),
            (true, "create graph", false),
            (true, "label", false),
            (false, "create graph", true),
        ] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                let name = if present { "g" } else { "missing" };
                if !present {
                    a.engine.create_graph(name).unwrap();
                }
                let result = a.sql(&format!("SELECT '{name}'::regnamespace::oid AS oid"));
                let Value::Int(oid) = result.rows[0]["oid"] else {
                    panic!("expected an OID");
                };
                if !present {
                    a.engine.drop_graph(name).unwrap();
                }
                let b = a.sibling();
                a.begin();
                b.begin();
                if warm {
                    a.sql("SELECT to_regtype('integer')::oid");
                }
                let result = a.sql(&format!("SELECT {oid}::regnamespace::text AS name"));
                assert_eq!(
                    result.rows[0]["name"],
                    Value::Str(if present {
                        name.into()
                    } else {
                        oid.to_string()
                    })
                );
                pivot(&a, &b);
                write(&b, write_kind);
                finish(&a, &b, conflict);
            }
        }
    }
}

#[test]
fn unused_catalog_scalars_and_cache_hydration_do_not_observe_graph_definitions_or_entities() {
    for query in [
        "SELECT to_regnamespace('missing') LIMIT 0",
        "EXPLAIN SELECT 'g'::regnamespace::text",
        "SELECT 'g'::regnamespace LIMIT 0",
        "SELECT to_regnamespace(NULL::text)",
        "SELECT 0::regnamespace::text",
        "SELECT to_regtype('integer')::oid",
        "SELECT 23::regtype::text",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql(query);
            pivot(&a, &b);
            write(&b, "create graph");
            write(&b, "label");
            write(&b, "vertex");
            finish(&a, &b, false);
        }
    }
}

#[test]
fn refreshed_and_nested_scalar_catalogs_keep_the_original_participant_across_providers() {
    use uqa_execution::catalog::projection::{resolve_regnamespace_oid, resolve_regobject_oid};
    use uqa_sql::ColumnType;
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        a.begin();
        let cache = RegtypeOutputCache::default();
        context(&a, &cache)
            .with_query_reads(|catalog| {
                assert!(resolve_regnamespace_oid(catalog, "g")?.is_some());
                let original = catalog.catalog_read_view();
                a.engine.commit().unwrap();
                a.begin();
                let original_error = original.read_graph_names().unwrap_err();
                let error = catalog
                    .catalog
                    .refreshed_catalog_snapshot()?
                    .read_graph_names()
                    .unwrap_err();
                assert_eq!(error.sqlstate(), original_error.sqlstate());
                assert_eq!(error.to_string(), original_error.to_string());
                let error = catalog
                    .with_query_reads(|nested| {
                        resolve_regobject_oid(nested, &ColumnType::Regnamespace, "g")
                    })
                    .unwrap_err();
                assert_eq!(error.sqlstate(), original_error.sqlstate());
                assert_eq!(error.to_string(), original_error.to_string());
                assert_eq!(
                    catalog
                        .catalog
                        .current_catalog_snapshot()
                        .read_graph_names()?,
                    ["g"]
                );
                a.engine.rollback().unwrap();
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn shared_scalar_catalog_cache_records_each_transaction_and_retains_savepoint_reads() {
    use uqa_execution::catalog::projection::resolve_regnamespace_oid;
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let cache = RegtypeOutputCache::default();
        a.begin();
        context(&a, &cache)
            .with_query_reads(|catalog| resolve_regnamespace_oid(catalog, "g"))
            .unwrap();
        a.engine.commit().unwrap();
        let b = a.sibling();
        a.begin();
        b.begin();
        a.sql("SAVEPOINT before_read");
        context(&a, &cache)
            .with_query_reads(|catalog| resolve_regnamespace_oid(catalog, "g"))
            .unwrap();
        a.sql("ROLLBACK TO SAVEPOINT before_read");
        pivot(&a, &b);
        write(&b, "drop graph");
        finish(&a, &b, true);
    }
}
