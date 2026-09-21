//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::roles::RoleDefinition;

fn catalog() -> CatalogReadView {
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
    snapshot.definitions.schemas = Arc::new(
        [
            (
                "public".into(),
                BoundSchemaSecurity::with_public_privileges(true),
            ),
            ("private".into(), BoundSchemaSecurity::bootstrap("private")),
        ]
        .into(),
    );
    snapshot.definitions.graphs =
        Arc::new([("g".into(), Arc::new(uqa_graph::GraphStoreHandle::default()))].into());
    CatalogReadView::new(snapshot)
}

fn resolution(path: &[&str]) -> RelationNameResolution {
    RelationNameResolution {
        search_path: path.iter().map(|name| (*name).into()).collect(),
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: super::super::RelationLookupMode::Dynamic,
    }
}

#[test]
fn effective_namespaces_preserve_order_implicit_catalog_and_duplicate_elision() {
    let catalog = catalog();
    let path = resolution(&["missing", "g", "public", "g", "public"]);
    assert_eq!(
        current_schema_name(&catalog, &path, "uqa").unwrap(),
        Some("g".into())
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", false).unwrap(),
        ["g", "public"]
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true).unwrap(),
        ["pg_catalog", "g", "public"]
    );
    let path = resolution(&["public", "pg_catalog", "g"]);
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true).unwrap(),
        ["public", "pg_catalog", "g"]
    );
}

#[test]
fn namespace_selection_respects_usage_and_keeps_empty_and_temporary_results() {
    let catalog = catalog();
    let path = resolution(&["missing", "private", "public"]);
    assert_eq!(
        current_schema_name(&catalog, &path, "reader").unwrap(),
        Some("public".into())
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "reader", false).unwrap(),
        ["public"]
    );
    let path = resolution(&["missing", "private"]);
    assert_eq!(
        current_schema_name(&catalog, &path, "reader").unwrap(),
        None
    );
    assert!(current_schema_names(&catalog, &path, "reader", false)
        .unwrap()
        .is_empty());
    assert_eq!(
        current_schema_names(&catalog, &path, "reader", true).unwrap(),
        ["pg_catalog"]
    );
    let mut temporary = resolution(&["pg_temp_1", "public"]);
    temporary.temporary_namespace_allocated = true;
    assert_eq!(
        current_schema_name(&catalog, &temporary, "reader").unwrap(),
        Some("pg_temp_1".into())
    );
}
