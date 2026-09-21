//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph namespace privilege inquiries observe the selected target under the original participant.

use super::{catalog_scalars::context, finish, fixtures, pivot};
use uqa_core::Value;
use uqa_execution::catalog::{
    cache::RegtypeOutputCache, security::schema_inquiry::has_schema_privilege_value,
};
use uqa_sql::catalog::security::schema_inquiry::SchemaPrivilegeInquiry;

#[test]
fn schema_privilege_overloads_observe_selected_graph_names_across_providers() {
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
            finish(&a, &b, true);
        }
    }
}

#[test]
fn namespace_privilege_queries_exclude_unrelated_graph_data_and_unused_targets() {
    for (query, write) in [
        ("SELECT has_schema_privilege('g', 'USAGE')", "other"),
        ("SELECT has_schema_privilege('g', 'USAGE')", "label"),
        ("SELECT has_schema_privilege('public', 'USAGE')", "drop"),
        ("SELECT has_schema_privilege('pg_catalog', 'USAGE')", "drop"),
        (
            "SELECT has_schema_privilege(NULL::name, 'g', 'USAGE')",
            "drop",
        ),
        ("SELECT has_schema_privilege(NULL::text, 'USAGE')", "drop"),
        ("SELECT has_schema_privilege('g', 'USAGE') LIMIT 0", "drop"),
        ("EXPLAIN SELECT has_schema_privilege('g', 'USAGE')", "drop"),
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
                "other" => {
                    b.engine.create_graph("other").unwrap();
                }
                "label" => {
                    b.engine
                        .create_graph_label("g", "p", uqa_graph::LabelKind::Vertex)
                        .unwrap();
                }
                "drop" => {
                    b.engine.drop_graph("g").unwrap();
                }
                _ => unreachable!(),
            }
            finish(&a, &b, false);
        }
    }
}

#[test]
fn namespace_privilege_absence_and_savepoint_reads_survive_across_providers() {
    for (absent, by_name) in [(false, false), (true, false), (true, true)] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            a.sql("CREATE ROLE namespace_reader");
            a.engine.create_graph("g").unwrap();
            let result = a.sql("SELECT 'g'::regnamespace::oid AS oid");
            let Value::Int(oid) = result.rows[0]["oid"] else {
                panic!("graph OID")
            };
            if absent {
                a.engine.drop_graph("g").unwrap();
            }
            let b = a.sibling();
            a.begin();
            b.begin();
            a.sql("SAVEPOINT namespace_read");
            if by_name {
                let error = a
                    .engine
                    .sql(
                        "SELECT has_schema_privilege('namespace_reader', 'g', 'USAGE')",
                        &[],
                    )
                    .unwrap_err();
                assert_eq!(error.sqlstate(), Some("3F000"));
            } else {
                assert_eq!(
                    a.sql(&format!("SELECT has_schema_privilege('namespace_reader', {oid}::oid, 'USAGE') AS allowed")).rows[0]["allowed"],
                    if absent { Value::Null } else { Value::Bool(false) }
                );
            }
            a.sql("ROLLBACK TO SAVEPOINT namespace_read");
            pivot(&a, &b);
            if absent {
                b.engine.create_graph("g").unwrap();
            } else {
                b.engine.drop_graph("g").unwrap();
            }
            finish(&a, &b, true);
        }
    }
}

#[test]
fn privilege_queries_distinguish_allocated_temporary_namespaces_from_graph_names() {
    use uqa_execution::catalog::services::CatalogSession;
    for allocated in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let name = CatalogSession::temporary_schema_name(&a.engine);
            let graph = if allocated {
                a.sql("CREATE TEMP TABLE temporary_namespace_marker (id INTEGER)");
                "g"
            } else {
                &name
            };
            a.engine.create_graph(graph).unwrap();
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(
                a.sql(&format!(
                    "SELECT has_schema_privilege('{name}', 'USAGE') AS allowed"
                ))
                .rows[0]["allowed"],
                Value::Bool(true)
            );
            pivot(&a, &b);
            b.engine.drop_graph(graph).unwrap();
            finish(&a, &b, !allocated);
        }
    }
}

#[test]
fn namespace_privilege_queries_preserve_participants_across_refresh_and_nesting() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        a.engine.create_graph("g").unwrap();
        a.begin();
        let cache = RegtypeOutputCache::default();
        let inquiry = SchemaPrivilegeInquiry {
            catalog: &a.engine,
            names: &a.engine,
            roles: &a.engine,
        };
        let arguments = [Value::Str("g".into()), Value::Str("USAGE".into())];
        context(&a, &cache)
            .with_query_reads(|catalog| {
                assert_eq!(
                    has_schema_privilege_value(catalog, &inquiry, &arguments)?,
                    Value::Bool(true)
                );
                let original = catalog.catalog_read_view();
                a.engine.commit().unwrap();
                a.begin();
                let original_error = original.read_graph_names().unwrap_err();
                let error = has_schema_privilege_value(catalog, &inquiry, &arguments).unwrap_err();
                assert_eq!(error.sqlstate(), original_error.sqlstate());
                assert_eq!(error.to_string(), original_error.to_string());
                assert_eq!(
                    has_schema_privilege_value(
                        catalog,
                        &inquiry,
                        &[Value::Null, Value::Str("USAGE".into())]
                    )?,
                    Value::Null
                );
                a.engine.rollback().unwrap();
                Ok(())
            })
            .unwrap();
    }
}
