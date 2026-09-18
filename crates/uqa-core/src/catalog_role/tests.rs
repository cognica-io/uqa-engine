//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bound_acl_requires_an_explicit_grantee_without_promoting_missing_data_to_public() {
    let entry = BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: true,
        grant_options: false,
    };
    let encoded = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        serde_json::from_value::<BoundAclEntry<bool>>(encoded.clone()).unwrap(),
        entry
    );
    for field in ["role", "grantor", "privileges", "grant_options"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<BoundAclEntry<bool>>(missing).is_err(),
            "{field}"
        );
    }
}
