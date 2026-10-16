//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn range_descriptors_share_both_oids_and_retain_their_generation_without_repeat_allocations() {
    let original = definition();
    let catalog = catalog(original.clone());
    let first = catalog
        .range_value_descriptor(original.oid)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &catalog
            .range_value_descriptor(original.multirange.oid)
            .unwrap()
            .unwrap()
    ));
    let cloned = catalog.clone();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..128 {
            assert!(Arc::ptr_eq(
                &first,
                &cloned
                    .range_value_descriptor(original.oid)
                    .unwrap()
                    .unwrap()
            ));
            assert!(cloned
                .range_value_descriptor(original.array_oid)
                .unwrap()
                .is_none());
        }
    });
    assert_eq!(allocation.count_total, 0);
    let empty = crate::catalog::test_support::empty_catalog();
    assert!(empty.range_descriptors.is_none());
    assert!(empty
        .range_value_descriptor(original.oid)
        .unwrap()
        .is_none());
    let mut replacement = definition();
    replacement.oid = 40_000;
    replacement.multirange.oid = 40_001;
    replacement.subtype = ColumnType::Text;
    let replacement = super::catalog(replacement);
    assert!(replacement
        .range_value_descriptor(original.oid)
        .unwrap()
        .is_none());
    assert_eq!(
        replacement
            .range_value_descriptor(40_000)
            .unwrap()
            .unwrap()
            .endpoint,
        ColumnType::Text
    );
    assert_eq!(first.endpoint, ColumnType::Integer);
}

#[test]
fn restored_ranges_reject_cross_family_identity_and_namespace_collisions() {
    let range = definition();
    for collision in 0..4 {
        let mut snapshot = super::catalog(range.clone()).snapshot().clone();
        let mut shell = uqa_sql::catalog::shell_type::StoredShellType {
            object_id: [8; 16],
            oid: 60_000,
            identity: RelationIdentity::new("public", "another"),
            owner: RoleIdentity::BOOTSTRAP,
            usage_acl: None,
        };
        match collision {
            0 => shell.oid = range.oid,
            1 => shell.oid = range.multirange.array_oid,
            2 => shell.identity.name.clone_from(&range.array_name),
            _ => shell
                .identity
                .name
                .clone_from(&range.multirange.identity.name),
        }
        snapshot.definitions.shells =
            Arc::new(BTreeMap::from([(shell.identity.qualified_name(), shell)]));
        assert!(
            validate_catalog(&crate::catalog::CatalogReadView::new(snapshot)).is_err(),
            "collision {collision}"
        );
    }
    let mut missing = range.clone();
    missing.subtype = range.multirange_type();
    assert!(validate_catalog(&super::catalog(missing)).is_err());
    validate_catalog(&super::catalog(range)).unwrap();
}

#[test]
fn range_claim_validation_borrows_unrelated_catalog_identities_without_projecting_rows() {
    use crate::catalog::{
        projection::TYPE_PROJECTION_BUILDS, test_support::table_snapshot, CatalogReadView,
    };
    for count in [0_u32, 512] {
        let mut snapshot = super::catalog(definition()).snapshot().clone();
        for index in 0..count {
            let name = format!("unrelated_{index}");
            let mut object_id = [9; 16];
            object_id[..4].copy_from_slice(&index.to_le_bytes());
            let mut table = table_snapshot(
                object_id,
                Vec::new(),
                uqa_sql::ast::TableConstraintSet::default(),
            );
            table.catalog_oids.relation = 200_000 + index;
            table.catalog_oids.row_type = Some(100_000 + index * 2);
            table.catalog_oids.array_type = Some(100_001 + index * 2);
            table.row_type_array_name = Some(format!("_{name}"));
            snapshot
                .tables
                .insert(RelationIdentity::new("public", name), table);
        }
        let catalog = CatalogReadView::new(snapshot);
        validate_catalog(&catalog).unwrap();
        let projections = TYPE_PROJECTION_BUILDS.get();
        let allocation = allocation_counter::measure(|| validate_catalog(&catalog).unwrap());
        assert_eq!(TYPE_PROJECTION_BUILDS.get(), projections);
        assert!(allocation.bytes_max < 16_384, "{count}: {allocation:?}");
    }
}
