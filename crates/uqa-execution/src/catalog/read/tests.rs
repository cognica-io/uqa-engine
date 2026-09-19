//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog snapshots resolve persistent and virtual names in one namespace.

use super::*;
use crate::catalog::{security::BoundTableSecurity, test_support::empty_catalog};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::VirtualRelation;

fn catalog() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    for schema in ["public", "pg_temp_1"] {
        snapshot.tables.insert(
            RelationIdentity::new(schema, "pg_class"),
            CatalogTableSnapshot {
                object_id: [1; 16],
                security: Arc::new(BoundTableSecurity::owner(
                    uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
                )),
                columns: Arc::default(),
                columns_declared: true,
                checks: Arc::default(),
                foreign_keys: Arc::default(),
                keys: Arc::default(),
                hierarchy: Arc::default(),
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
            },
        );
    }
    CatalogReadView::new(snapshot)
}

fn resolution(path: &[&str], mode: RelationLookupMode) -> RelationNameResolution {
    RelationNameResolution {
        search_path: path.iter().map(|schema| (*schema).into()).collect(),
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: true,
        current_user: "uqa".into(),
        lookup_mode: mode,
    }
}

#[test]
fn virtual_and_physical_readers_stop_at_the_same_first_namespace_match() {
    let catalog = catalog();
    for (path, canonical, virtual_relation) in [
        (vec!["public"], "pg_temp_1.pg_class", None),
        (
            vec!["public", "pg_catalog", "pg_temp"],
            "public.pg_class",
            None,
        ),
        (
            vec!["pg_catalog", "public", "pg_temp"],
            "pg_catalog.pg_class",
            Some(VirtualRelation::PgClass),
        ),
    ] {
        let resolution = resolution(&path, RelationLookupMode::Dynamic);
        assert_eq!(
            catalog
                .relation_kind_resolution(&resolution, "pg_class")
                .unwrap(),
            RelationResolution::Found(canonical.into(), "table")
        );
        assert_eq!(
            catalog
                .virtual_relation_resolved(&resolution, "pg_class")
                .unwrap(),
            virtual_relation
        );
        assert_eq!(
            catalog
                .table_resolved(&resolution, "pg_class")
                .unwrap()
                .is_none(),
            virtual_relation.is_some()
        );
        assert_eq!(
            catalog
                .table_name_resolved(&resolution, "pg_class")
                .unwrap(),
            virtual_relation.is_none().then(|| canonical.into())
        );
    }
}

#[test]
fn bound_system_relations_ignore_shadowing_and_reject_unqualified_bindings() {
    let catalog = catalog();
    let resolution = resolution(&["public"], RelationLookupMode::Bound);
    assert_eq!(
        catalog
            .virtual_relation_resolved(&resolution, "pg_catalog.pg_class")
            .unwrap(),
        Some(VirtualRelation::PgClass)
    );
    assert!(catalog
        .table_resolved(&resolution, "pg_catalog.pg_class")
        .unwrap()
        .is_none());
    assert!(catalog
        .virtual_relation_resolved(&resolution, "pg_class")
        .is_err());
    assert!(catalog
        .virtual_relation_resolved(&resolution, "public.pg_class")
        .unwrap()
        .is_none());
}
