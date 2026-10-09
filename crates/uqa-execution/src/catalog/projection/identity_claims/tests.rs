//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::projection::pg_catalog::TYPE_PROJECTION_BUILDS;
use crate::catalog::{test_support::empty_catalog, RelationLookupMode};
use std::sync::Arc;
use uqa_sql::catalog::security::BoundSchemaSecurity;

mod types;

#[test]
fn oid_initialization_excludes_bootstrap_namespaces_but_observes_user_claims() {
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Bound,
    };
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = Arc::new(BoundSchemaSecurity::initial_catalog());
    assert_eq!(
        largest_catalog_oid(&CatalogReadView::new(snapshot.clone()), &resolution).unwrap(),
        None
    );
    Arc::make_mut(&mut snapshot.definitions.schemas).insert(
        "retained".into(),
        BoundSchemaSecurity::bootstrap_with_oid("retained", 70_000),
    );
    assert_eq!(
        largest_catalog_oid(&CatalogReadView::new(snapshot), &resolution).unwrap(),
        Some(70_000)
    );
}

fn resolution() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: crate::catalog::RelationLookupMode::Bound,
    }
}

#[test]
fn type_occupancy_reads_identities_without_building_type_rows() {
    let catalog = empty_catalog();
    let resolution = resolution();
    let before = TYPE_PROJECTION_BUILDS.get();
    for _ in 0..4 {
        assert!(catalog_oid_in_use(&catalog, &resolution, CatalogOidClass::Type, 23).unwrap());
        assert!(!catalog_oid_in_use(&catalog, &resolution, CatalogOidClass::Type, 99_999).unwrap());
    }
    assert_eq!(TYPE_PROJECTION_BUILDS.get() - before, 0);
}
