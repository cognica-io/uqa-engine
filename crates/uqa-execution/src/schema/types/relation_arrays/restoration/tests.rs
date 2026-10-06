//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{test_support, CatalogTableSnapshot};
use std::sync::Arc;
use uqa_sql::catalog::relation_oids::RelationOidKind;

fn table(object_id: [u8; 16], name: Option<&str>, array_oid: Option<u32>) -> CatalogTableSnapshot {
    let mut oids = RelationCatalogOids::legacy(RelationOidKind::Table, &object_id);
    oids.array_type = array_oid;
    CatalogTableSnapshot {
        dropped_attributes: std::sync::Arc::new(Vec::new()),
        object_id,
        catalog_oids: oids,
        row_type_array_name: name.map(str::to_owned),
        security: Arc::new(crate::catalog::security::BoundTableSecurity::owner(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        )),
        columns: Arc::default(),
        columns_declared: true,
        checks: Arc::default(),
        foreign_keys: Arc::default(),
        keys: Arc::default(),
        hierarchy: Arc::default(),
        persistence: uqa_sql::ast::RelationPersistence::Permanent,
    }
}

fn resolution() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "postgres".into(),
        lookup_mode: crate::catalog::RelationLookupMode::Bound,
    }
}

#[test]
fn missing_array_preserves_scalar_identity_and_skips_occupied_oids_and_names() {
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    let relation = RelationIdentity::new("public", "item");
    let legacy = table([1; 16], None, None);
    let original = legacy.catalog_oids;
    snapshot.tables.insert(relation.clone(), legacy);
    snapshot.tables.insert(
        RelationIdentity::new("public", "_item"),
        table([2; 16], Some("__item"), Some(42_000)),
    );
    let catalog = CatalogReadView::new(snapshot);
    let mut candidates = [original.row_type.unwrap(), 42_000, 42_001].into_iter();
    let changes = prepare(&catalog, &resolution(), &mut || {
        Ok(candidates.next().unwrap())
    })
    .unwrap();
    let (upgraded, name) = &changes[&relation];
    assert_eq!(upgraded.relation, original.relation);
    assert_eq!(upgraded.row_type, original.row_type);
    assert_eq!(upgraded.rule, original.rule);
    assert_eq!(upgraded.array_type, Some(42_001));
    assert_eq!(name, "_item_1");
    assert_eq!(
        catalog.snapshot().tables[&relation].catalog_oids.array_type,
        None
    );
    assert_eq!(
        catalog.snapshot().tables[&relation].row_type_array_name,
        None
    );
}

#[test]
fn missing_array_name_keeps_its_recorded_identity_and_reopen_is_idempotent() {
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    let relation = RelationIdentity::new("public", "item");
    snapshot
        .tables
        .insert(relation.clone(), table([3; 16], None, Some(45_000)));
    let original = snapshot.tables[&relation].catalog_oids;
    let catalog = CatalogReadView::new(snapshot.clone());
    let changes = prepare(&catalog, &resolution(), &mut || {
        panic!("existing OIDs cannot be allocated again")
    })
    .unwrap();
    assert_eq!(changes[&relation], (original, "_item".into()));
    let state = snapshot.tables.get_mut(&relation).unwrap();
    state.row_type_array_name = Some(changes[&relation].1.clone());
    let reopened = CatalogReadView::new(snapshot);
    assert!(prepare(&reopened, &resolution(), &mut || panic!(
        "completed migration cannot allocate"
    ))
    .unwrap()
    .is_empty());
    assert_eq!(
        catalog.snapshot().tables[&relation].row_type_array_name,
        None
    );
}

#[test]
fn recorded_displaced_arrays_keep_their_name_and_block_new_default_names() {
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "original"),
        table([4; 16], Some("_other"), Some(45_000)),
    );
    let other = RelationIdentity::new("public", "other");
    snapshot
        .tables
        .insert(other.clone(), table([5; 16], None, Some(45_001)));
    let changes = prepare(&CatalogReadView::new(snapshot), &resolution(), &mut || {
        panic!("already recorded")
    })
    .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[&other].1, "_other_1");
}

#[test]
fn malformed_recorded_names_fail_before_allocations_or_catalog_writes() {
    for malformed in [String::new(), "x".repeat(64), "a\0b".into(), "item".into()] {
        let mut snapshot = test_support::empty_catalog().snapshot().clone();
        snapshot.tables.insert(
            RelationIdentity::new("public", "item"),
            table([6; 16], Some(&malformed), Some(46_000)),
        );
        snapshot.tables.insert(
            RelationIdentity::new("public", "legacy"),
            table([7; 16], None, None),
        );
        let catalog = CatalogReadView::new(snapshot);
        assert!(validate(&catalog).is_err(), "{malformed:?}");
        assert!(
            prepare(&catalog, &resolution(), &mut || panic!(
                "validate the complete snapshot before allocating"
            ))
            .is_err(),
            "{malformed:?}"
        );
    }
}

#[test]
fn duplicate_arrays_and_explicit_type_collisions_are_rejected() {
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "first"),
        table([8; 16], Some("_same"), Some(47_000)),
    );
    snapshot.tables.insert(
        RelationIdentity::new("public", "second"),
        table([9; 16], Some("_same"), Some(47_001)),
    );
    assert!(validate(&CatalogReadView::new(snapshot.clone())).is_err());
    snapshot
        .tables
        .remove(&RelationIdentity::new("public", "second"));
    Arc::make_mut(&mut snapshot.definitions.enums).insert(
        "public._same".into(),
        uqa_sql::catalog::enum_type::StoredEnum {
            object_id: [10; 16],
            oid: 47_002,
            array_oid: 47_003,
            array_name: "__same".into(),
            identity: RelationIdentity::new("public", "_same"),
            owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            labels: Vec::new(),
            usage_acl: None,
        },
    );
    assert!(validate(&CatalogReadView::new(snapshot)).is_err());
}
