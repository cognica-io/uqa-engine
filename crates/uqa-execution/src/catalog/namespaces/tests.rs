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
        current_schema_name(&catalog, &path, "uqa"),
        Some("g".into())
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", false),
        ["g", "public"]
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["pg_catalog", "g", "public"]
    );
    let path = resolution(&["public", "pg_catalog", "g"]);
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["public", "pg_catalog", "g"]
    );
}

#[test]
fn namespace_selection_respects_usage_and_keeps_empty_and_temporary_results() {
    let catalog = catalog();
    let path = resolution(&["missing", "private", "public"]);
    assert_eq!(
        current_schema_name(&catalog, &path, "reader"),
        Some("public".into())
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "reader", false),
        ["public"]
    );
    let path = resolution(&["missing", "private"]);
    assert_eq!(current_schema_name(&catalog, &path, "reader"), None);
    assert!(current_schema_names(&catalog, &path, "reader", false).is_empty());
    assert_eq!(
        current_schema_names(&catalog, &path, "reader", true),
        ["pg_catalog"]
    );
    let mut temporary = resolution(&["pg_temp_1", "public"]);
    temporary.temporary_namespace_allocated = true;
    assert_eq!(
        current_schema_name(&catalog, &temporary, "reader"),
        Some("pg_temp_1".into())
    );
}

#[test]
fn the_temporary_namespace_leads_the_implicit_schemas_once_it_exists() {
    let catalog = catalog();
    let mut path = resolution(&["public"]);
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["pg_catalog", "public"]
    );
    path.temporary_namespace_allocated = true;
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["pg_temp_1", "pg_catalog", "public"]
    );
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", false),
        ["public"]
    );
    // An explicit `pg_temp` keeps its position and stands for the namespace only once it exists.
    let mut path = resolution(&["public", "pg_temp"]);
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["pg_catalog", "public"]
    );
    path.temporary_namespace_allocated = true;
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", true),
        ["pg_catalog", "public", "pg_temp_1"]
    );
    let mut path = resolution(&["pg_temp", "public"]);
    assert_eq!(
        current_schema_name(&catalog, &path, "uqa"),
        Some("public".into())
    );
    path.temporary_namespace_allocated = true;
    assert_eq!(
        current_schema_name(&catalog, &path, "uqa"),
        Some("pg_temp_1".into())
    );
    // The namespace's own name stands for it only once it exists.
    let path = resolution(&["pg_temp_1", "public"]);
    assert_eq!(
        current_schema_names(&catalog, &path, "uqa", false),
        ["public"]
    );
}

#[test]
fn analysis_paths_compare_usable_namespaces_and_the_creation_target() {
    use uqa_sql::catalog::analysis::AnalysisCatalog;

    let catalog = catalog();
    let path = |resolution: &RelationNameResolution| {
        catalog.effective_search_path(resolution).unwrap().unwrap()
    };
    let public = path(&resolution(&["public"]));
    assert_eq!(public.schemas, ["pg_catalog", "public"]);
    assert_eq!(public.creation_namespace.as_deref(), Some("public"));
    let explicit_catalog = path(&resolution(&["pg_catalog", "public"]));
    assert_eq!(explicit_catalog.schemas, public.schemas);
    assert_ne!(explicit_catalog, public);
    assert_eq!(path(&resolution(&["missing", "public", "public"])), public);
    let mut denied = resolution(&["private", "public"]);
    denied.current_user = "reader".into();
    assert_eq!(path(&denied), public);
    let mut temporary = resolution(&["public"]);
    temporary.temporary_namespace_allocated = true;
    assert_ne!(path(&temporary), public);
}
