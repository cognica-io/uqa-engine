//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::TableConstraintSet;
use crate::schema::constraint_metadata::{
    materialize_constraint_metadata, ConstraintMetadataResult,
};

fn declaration() -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE parent(id int CONSTRAINT positive CHECK(id > 0), CONSTRAINT unique_id UNIQUE(id), CONSTRAINT upper_bound CHECK(id < 100))").unwrap().remove(0) else { panic!("table declaration") };
    (
        table.columns,
        TableConstraintSet {
            key_constraints: table.key_constraints,
            checks: table.checks,
            ..Default::default()
        },
    )
}

fn allocator() -> impl FnMut(&str) -> ConstraintMetadataResult<[u8; 16]> {
    let mut next = 0;
    move |_| {
        next += 1;
        Ok([next; 16])
    }
}

#[test]
fn key_and_check_catalog_identities_survive_name_changes_and_are_retired_on_recreation() {
    let (mut columns, mut constraints) = declaration();
    let mut allocate = allocator();
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "parent"),
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .unwrap();
    let before = identities(&columns, &constraints);
    assert_eq!(before.len(), 3);
    assert!(before.iter().all(|identity| identity.is_valid()));
    constraints.key_constraints[0].name = Some("renamed_key".into());
    constraints.checks[0].name = Some("renamed_check".into());
    columns[0].check_name = Some("renamed_column_check".into());
    assert!(!materialize_constraint_metadata(
        &RelationIdentity::new("moved", "renamed"),
        &mut columns,
        &mut constraints,
        &mut allocate
    )
    .unwrap());
    assert_eq!(identities(&columns, &constraints), before);
    constraints.key_constraints[0].catalog_identity = None;
    columns[0].check_object_id = None;
    columns[0].check_catalog_oid = None;
    constraints.checks[0].object_id = None;
    constraints.checks[0].catalog_oid = None;
    materialize_constraint_metadata(
        &RelationIdentity::new("moved", "renamed"),
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .unwrap();
    assert!(identities(&columns, &constraints)
        .iter()
        .all(|identity| !before.contains(identity)));
}

#[test]
fn legacy_key_and_check_conversion_preserves_existing_public_addresses_once() {
    let (mut columns, mut constraints) = declaration();
    columns[0].check_object_id = Some([90; 16]);
    let relation = RelationIdentity::new("public", "parent");
    let legacy = super::super::legacy::LegacyIdentities::capture(&columns, &constraints);
    let mut allocate = allocator();
    materialize_constraint_metadata(&relation, &mut columns, &mut constraints, &mut allocate)
        .unwrap();
    assert_eq!(
        legacy
            .preserve_oids(&relation, &mut columns, &mut constraints)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        columns[0].check_catalog_oid,
        Some(crate::catalog::oids::stable_object_oid(
            "constraint",
            &[90; 16]
        ))
    );
    assert_eq!(
        constraints.key_constraints[0].catalog_identity.unwrap().oid,
        crate::catalog::oids::stable_oid("constraint", "public.parent.unique_id")
    );
    assert_eq!(
        constraints.checks[0].catalog_oid,
        Some(crate::catalog::oids::stable_oid(
            "constraint",
            "public.parent.upper_bound"
        ))
    );
    validate_constraint_identities(&columns, &constraints).unwrap();
    let current = super::super::legacy::LegacyIdentities::capture(&columns, &constraints);
    assert!(current
        .preserve_oids(&relation, &mut columns, &mut constraints)
        .unwrap()
        .is_empty());
}

#[test]
fn attached_key_provenance_tracks_local_incarnation_through_renaming() {
    let (mut columns, mut parent) = declaration();
    let mut allocate = allocator();
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "parent"),
        &mut columns,
        &mut parent,
        &mut allocate,
    )
    .unwrap();
    let mut child = TableConstraintSet::default();
    child.hierarchy.partition_inherited_key_constraints =
        crate::schema::inheritance::alter::append_inherited_keys(
            &mut child.key_constraints,
            &parent.key_constraints,
        );
    assert!(child.key_constraints[0].catalog_identity.is_none());
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "child"),
        &mut [],
        &mut child,
        &mut allocate,
    )
    .unwrap();
    assert_ne!(
        child.key_constraints[0].catalog_identity,
        parent.key_constraints[0].catalog_identity
    );
    assert_eq!(
        child.key_constraints,
        child.hierarchy.partition_inherited_key_constraints
    );
    child.key_constraints[0].name = Some("local_renamed".into());
    validate_constraint_identities(&[], &child).unwrap();
    let replacement = parent.key_constraints[0].clone();
    child.key_constraints.push(replacement.clone());
    crate::schema::inheritance::alter::clear_partition_constraint_provenance(&mut child);
    assert_eq!(child.key_constraints.len(), 2);
    assert_eq!(child.key_constraints[1], replacement);
    assert!(child
        .hierarchy
        .partition_inherited_key_constraints
        .is_empty());
}

#[test]
fn temporal_keys_do_not_reuse_an_ordinary_unique_constraint() {
    let (_, constraints) = declaration();
    let mut target = constraints.key_constraints.clone();
    let mut inherited = constraints.key_constraints;
    inherited[0].without_overlaps = true;
    assert_eq!(
        crate::schema::inheritance::alter::append_inherited_keys(&mut target, &inherited).len(),
        1
    );
    assert_eq!(target.len(), 2);
}

#[test]
fn current_key_and_check_metadata_rejects_missing_or_cross_kind_duplicate_identities() {
    let (mut columns, mut constraints) = declaration();
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "parent"),
        &mut columns,
        &mut constraints,
        &mut allocator(),
    )
    .unwrap();
    let identity = constraints.key_constraints[0].catalog_identity.unwrap();
    let mut missing = constraints.clone();
    missing.key_constraints[0].catalog_identity = None;
    assert!(validate_constraint_identities(&columns, &missing).is_err());
    let mut missing = columns.clone();
    missing[0].check_catalog_oid = None;
    assert!(validate_constraint_identities(&missing, &constraints).is_err());
    let mut duplicate = constraints.clone();
    duplicate.checks[0].catalog_oid = Some(identity.oid);
    assert!(validate_constraint_identities(&columns, &duplicate).is_err());
    duplicate.checks[0].catalog_oid = constraints.checks[0].catalog_oid;
    duplicate.checks[0].object_id = Some(identity.object_id);
    assert!(validate_constraint_identities(&columns, &duplicate).is_err());
    columns[0].check = None;
    assert!(validate_constraint_identities(&columns, &constraints).is_err());
}

#[test]
fn taking_an_absent_column_check_does_not_mutate_its_metadata() {
    let (mut columns, _) = declaration();
    columns[0].check = None;
    columns[0].check_catalog_oid = Some(24_000);
    assert!(crate::schema::constraint_changes::take_column_check(&mut columns[0]).is_none());
    assert_eq!(columns[0].check_catalog_oid, Some(24_000));
}

#[test]
fn allocator_errors_preserve_cancellation_and_serialization_sqlstates() {
    for code in ["40001", "57014"] {
        let (mut columns, mut constraints) = declaration();
        let mut allocate = |_: &str| {
            Err(
                crate::schema::constraint_metadata::ConstraintMetadataError::Execution(Box::new(
                    crate::SQLError::Routine {
                        sqlstate: code.into(),
                        message: "reservation failed".into(),
                    },
                )),
            )
        };
        let failure = materialize_constraint_metadata(
            &RelationIdentity::new("public", "parent"),
            &mut columns,
            &mut constraints,
            &mut allocate,
        )
        .unwrap_err();
        assert_eq!(
            crate::catalog::errors::storage_error("identity", &failure).sqlstate(),
            Some(code)
        );
    }
}
