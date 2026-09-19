//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::catalog_role::RoleIdentity;

#[test]
fn tuple_replacements_distinguish_equal_acl_values_and_preserve_siblings() {
    let mut security = BoundRelationSecurity::owner(RoleIdentity::BOOTSTRAP);
    let acl = vec![BoundAclEntry {
        role: None,
        grantor: security.role_owner,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    }];
    let first = RelationAclTuple::new(Some(acl.clone())).unwrap();
    let equal = RelationAclTuple::new(Some(acl)).unwrap();
    assert_ne!(first.revision, equal.revision);
    first.apply(Some("a"), &mut security).unwrap();
    equal.apply(Some("b"), &mut security).unwrap();
    let cleared = RelationAclTuple::new(Some(Vec::new())).unwrap();
    cleared.apply(Some("a"), &mut security).unwrap();
    assert!(!security.column_acls.contains_key("a"));
    assert!(security.column_acls.contains_key("b"));
    assert_eq!(
        security.acl_revisions.get(Some("a")),
        Some(cleared.revision)
    );
    assert_eq!(security.acl_revisions.get(Some("b")), Some(equal.revision));
    assert_eq!(security.acl, None);
}

#[test]
fn names_and_tuple_kinds_have_unambiguous_keys() {
    let relation = RelationIdentity::new("a:b\0", "c\"d");
    let other = RelationIdentity::new("a", "b\0:c\"d");
    for column_name in [None, Some(""), Some("null"), Some("a:b\0\"%")] {
        let encoded = key(&relation, column_name);
        assert_eq!(
            column(&prefix(&relation), &encoded).unwrap().as_deref(),
            column_name
        );
        assert!(!encoded.starts_with(&prefix(&other)));
    }
    assert_ne!(key(&relation, None), key(&relation, Some("null")));
}

#[test]
fn corrupt_tuple_versions_and_null_attribute_acls_are_rejected() {
    let tuple = RelationAclTuple::new(None).unwrap();
    let encoded = serde_json::to_value(&tuple).unwrap();
    for field in ["format", "revision", "acl"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(RelationAclTuple::decode(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    let mut zero = tuple.clone();
    zero.revision = [0; 16];
    assert!(RelationAclTuple::decode(&serde_json::to_vec(&zero).unwrap()).is_err());
    let mut security = BoundRelationSecurity::owner(RoleIdentity::BOOTSTRAP);
    assert!(tuple.apply(Some("a"), &mut security).is_err());
    tuple.apply(None, &mut security).unwrap();
}
