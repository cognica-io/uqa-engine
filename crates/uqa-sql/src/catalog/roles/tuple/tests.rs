//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::RoleDefinitions;

fn role() -> RoleDefinition {
    let mut role = RoleDefinition::bootstrap();
    role.name = "target".into();
    role.oid = 20_001;
    role.object_id = [1; 16];
    role
}

#[test]
fn role_tuple_changes_reject_updated_or_replaced_versions_without_rebinding() {
    let original = role();
    let bound = RoleTuple::bind(&original).unwrap();
    let mut roles = RoleDefinitions::try_from_iter([("target".into(), original.clone())]).unwrap();
    assert_eq!(bound.revalidate(&roles).unwrap(), &original);
    roles
        .edit("target", |value| {
            value.advance_revision().unwrap();
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(
        matches!(bound.revalidate(&roles), Err(SQLError::Routine { sqlstate, message }) if sqlstate == "XX000" && message == "tuple concurrently updated")
    );
    let mut renamed = roles.get("target").unwrap().clone();
    roles.remove("target").unwrap();
    renamed.name = "renamed".into();
    roles.insert(renamed.name.clone(), renamed).unwrap();
    assert!(
        matches!(bound.revalidate(&roles), Err(SQLError::Routine { sqlstate, message }) if sqlstate == "XX000" && message == "tuple concurrently updated")
    );
    roles.clear();
    let mut replacement = original;
    replacement.object_id = [2; 16];
    roles.insert(replacement.name.clone(), replacement).unwrap();
    assert!(
        matches!(bound.revalidate(&roles), Err(SQLError::Routine { sqlstate, message }) if sqlstate == "XX000" && message == "tuple concurrently deleted")
    );
}

#[test]
fn advancing_a_role_tuple_preserves_authority_and_detects_exhaustion() {
    let mut role = role();
    let original = role.clone();
    role.advance_revision().unwrap();
    assert_eq!(role.identity(), original.identity());
    assert_eq!(role.attributes, original.attributes);
    assert_eq!(role.revision, 2);
    for invalid in [0, u64::MAX] {
        role.revision = invalid;
        assert!(role.advance_revision().is_err());
        assert_eq!(role.revision, invalid);
    }
}

#[test]
fn publication_requires_one_new_tuple_version_for_each_changed_definition() {
    let original = role();
    let before = RoleDefinitions::try_from_iter([("target".into(), original.clone())]).unwrap();
    validate_revision_changes(&before, &before).unwrap();
    let mut changed = original.clone();
    changed.connection_limit = 3;
    let mut after = RoleDefinitions::try_from_iter([("target".into(), changed.clone())]).unwrap();
    assert!(validate_revision_changes(&before, &after).is_err());
    changed.advance_revision().unwrap();
    after.insert("target".into(), changed.clone()).unwrap();
    validate_revision_changes(&before, &after).unwrap();
    changed.advance_revision().unwrap();
    after.insert("target".into(), changed).unwrap();
    assert!(validate_revision_changes(&before, &after).is_err());
    validate_revision_changes(&before, &RoleDefinitions::new()).unwrap();
    validate_revision_changes(&RoleDefinitions::new(), &before).unwrap();
    assert!(validate_revision_changes(&RoleDefinitions::new(), &after).is_err());
}

#[test]
fn legacy_revision_markers_are_not_valid_current_definition_tuples() {
    let mut value = serde_json::to_value(role()).unwrap();
    value.as_object_mut().unwrap().remove("revision");
    let legacy: RoleDefinition = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.revision, 0);
    assert!(RoleTuple::bind(&legacy).is_err());
    assert!(validate_revisions(
        &RoleDefinitions::try_from_iter([("target".into(), legacy)]).unwrap()
    )
    .is_err());
}
