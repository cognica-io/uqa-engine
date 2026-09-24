//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    catalog_sequence::SequencePrivileges,
};

fn row(security: SequenceSecurityRow) -> crate::SequenceRow {
    crate::SequenceRow {
        relation: crate::RelationIdentity::new("public", "ids"),
        security,
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: 5,
        called: true,
        log_count: 3,
        persistence: "p".into(),
        owner: None,
        options: crate::SequenceOptions::default(),
    }
}

#[test]
fn sequence_records_round_trip_bound_and_legacy_authority_without_mixing_them() {
    let mut bound = BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP);
    bound.acl = Some(vec![BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }]);
    for security in [
        SequenceSecurityRow::Bound(bound),
        SequenceSecurityRow::bootstrap(),
        SequenceSecurityRow::legacy("owner"),
    ] {
        let original = row(security.clone());
        let encoded = serde_json::to_value(&original).unwrap();
        assert_eq!(
            serde_json::from_value::<crate::SequenceRow>(encoded.clone()).unwrap(),
            original
        );
        assert_eq!(
            encoded.get("sequence_security_format").is_some(),
            matches!(security, SequenceSecurityRow::Bound(_))
        );
        let mut corrupt = encoded;
        corrupt["unexpected_security_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<crate::SequenceRow>(corrupt).is_err());
    }
}

#[test]
fn current_sequence_marker_and_required_endpoints_never_fall_back_to_names() {
    let original = serde_json::to_value(row(SequenceSecurityRow::bootstrap())).unwrap();
    assert!(original.get("acl").is_some());
    for field in ["role_owner", "acl"] {
        let mut incomplete = original.clone();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<crate::SequenceRow>(incomplete).is_err(),
            "{field}"
        );
    }
    for value in [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!("1"),
    ] {
        let mut unsupported = original.clone();
        unsupported["sequence_security_format"] = value;
        assert!(serde_json::from_value::<crate::SequenceRow>(unsupported).is_err());
    }
    let mut mixed = original.clone();
    mixed["role_owner"] = serde_json::json!("uqa");
    assert!(serde_json::from_value::<crate::SequenceRow>(mixed).is_err());
    let mut unmarked = original;
    unmarked
        .as_object_mut()
        .unwrap()
        .remove("sequence_security_format");
    assert!(serde_json::from_value::<crate::SequenceRow>(unmarked).is_err());
}

#[test]
fn missing_bound_grantee_is_corrupt_while_explicit_null_is_public() {
    let mut security = BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(vec![BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }]);
    let encoded = serde_json::to_value(row(security.into())).unwrap();
    assert!(serde_json::from_value::<crate::SequenceRow>(encoded.clone()).is_ok());
    for field in ["role", "grantor"] {
        let mut incomplete = encoded.clone();
        incomplete["acl"][0].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<crate::SequenceRow>(incomplete).is_err(),
            "{field}"
        );
    }
}

#[test]
fn historic_sequences_without_authority_remain_legacy_until_initial_restoration() {
    let mut encoded = serde_json::to_value(row(SequenceSecurityRow::legacy("uqa"))).unwrap();
    encoded.as_object_mut().unwrap().remove("role_owner");
    let restored: crate::SequenceRow = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored.security, SequenceSecurityRow::legacy("uqa"));
}
