//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` namespace, ACL and missing-object diagnostics for both collation input forms.

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache, security::BoundSchemaSecurity, test_support::CatalogServices,
    CatalogReadView,
};
use std::sync::Arc;
use uqa_sql::catalog::roles::RoleDefinition;

#[derive(serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

#[derive(serde::Deserialize)]
struct Case {
    state: String,
    mode: String,
    name: String,
    oid: Option<i64>,
    error: Option<Diagnostic>,
}

fn catalog(revoked: bool) -> CatalogReadView {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 42;
    reader.object_id = [42; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(
        [
            ("uqa".into(), RoleDefinition::bootstrap()),
            ("reader".into(), reader),
        ]
        .into(),
    );
    let mut schemas = std::collections::BTreeMap::from([
        (
            "public".into(),
            BoundSchemaSecurity::with_public_privileges(true),
        ),
        ("private".into(), BoundSchemaSecurity::bootstrap("private")),
    ]);
    if revoked {
        schemas.insert(
            "pg_catalog".into(),
            BoundSchemaSecurity::owner(uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP),
        );
    }
    snapshot.definitions.schemas = Arc::new(schemas);
    CatalogReadView::new(snapshot)
}

#[test]
fn collation_namespace_selection_matches_postgresql_expression_and_regtype_rules() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../collations_namespaces.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let catalog = catalog(case.state == "catalog_revoked");
        let mut services = CatalogServices::default();
        if !matches!(case.state.as_str(), "owner" | "temporary") {
            services.resolution.current_user = "reader".into();
        }
        match case.state.as_str() {
            "temporary" => services.resolution.temporary_namespace_allocated = true,
            "empty_path" => services.resolution.search_path.clear(),
            "private_path" => services.resolution.search_path = vec!["private".into()],
            _ => {}
        }
        let cache = RegtypeOutputCache::default();
        let context = services.context(&catalog, &cache);
        let observed = match case.mode.as_str() {
            "expression" => resolve_expression_collation_oid(&context, &case.name)
                .map(|oid| Some(i64::from(oid))),
            "regcollation" => resolve_regcollation_oid(&context, &case.name),
            "lookup" => lookup_regcollation_oid(&context, &case.name),
            _ => panic!("reference mode"),
        };
        let label = format!("{} {} {}", case.state, case.mode, case.name);
        match (observed, case.error) {
            (Ok(oid), None) if oid == case.oid => {}
            (Err(actual), Some(expected))
                if actual.sqlstate() == Some(expected.sqlstate.as_str())
                    && actual.to_string() == expected.message
                    && actual.detail() == expected.detail.as_deref()
                    && actual.hint() == expected.hint.as_deref() => {}
            (actual, _) => failures.push(format!("{label}: {actual:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
