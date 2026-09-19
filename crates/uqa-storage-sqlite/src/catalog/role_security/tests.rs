//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn namespace_tuple_envelope_roundtrips_and_rejects_corruption_without_legacy_fallback() {
    let mut bound = BoundSchemaRow::bootstrap("s");
    bound.tuple = Some(SchemaTupleIdentity {
        oid: i64::from(u32::MAX),
        object_id: [7; 16],
        revision: [8; 16],
    });
    let row = SchemaRow::Bound(bound);
    let (owner, acl) = encode_schema(&row).unwrap();
    assert_eq!(
        decode_schema("s".into(), (&owner).into(), acl.as_deref()).unwrap(),
        row
    );
    let Value::Blob(original) = owner else {
        unreachable!()
    };
    assert_eq!(original.len(), 57);
    for length in [0, 20, 21, 25, 41, 56] {
        assert!(decode_schema("s".into(), ValueRef::Blob(&original[..length]), None).is_err());
    }
    for field in [0..1, 1..5, 5..21, 21..25, 25..41, 41..57] {
        let mut invalid = original.clone();
        invalid[field].fill(0);
        assert!(decode_schema("s".into(), ValueRef::Blob(&invalid), None).is_err());
    }
    assert!(decode_sequence(ValueRef::Blob(&original), None).is_err());
}

#[test]
fn sequence_cells_keep_role_storage_classes_and_reject_mixed_acl_endpoints() {
    use uqa_core::catalog_role::BoundAclEntry;
    use uqa_core::catalog_sequence::{
        BoundSequenceSecurity, LegacySequenceSecurity, SequenceAclEntry, SequencePrivileges,
    };
    use uqa_storage::SequenceSecurityRow;
    let owner = RoleIdentity::BOOTSTRAP;
    let mut bound = BoundSequenceSecurity::owner(owner);
    bound.acl = Some(vec![BoundAclEntry {
        role: None,
        grantor: owner,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }]);
    let mut legacy = LegacySequenceSecurity::owner(serde_json::to_string(&owner).unwrap());
    legacy.acl = Some(vec![SequenceAclEntry {
        role: "reader".into(),
        grantor: Some(legacy.role_owner.clone()),
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }]);
    let (bound_owner, bound_acl) =
        encode_sequence(&SequenceSecurityRow::Bound(bound.clone())).unwrap();
    let (legacy_owner, legacy_acl) =
        encode_sequence(&SequenceSecurityRow::Legacy(legacy.clone())).unwrap();
    assert!(matches!(bound_owner, Value::Blob(_)));
    assert!(matches!(legacy_owner, Value::Text(_)));
    assert_eq!(
        decode_sequence((&bound_owner).into(), bound_acl.as_deref()).unwrap(),
        SequenceSecurityRow::Bound(bound)
    );
    assert_eq!(
        decode_sequence((&legacy_owner).into(), legacy_acl.as_deref()).unwrap(),
        SequenceSecurityRow::Legacy(legacy)
    );
    assert!(decode_sequence((&bound_owner).into(), legacy_acl.as_deref()).is_err());
    assert!(decode_sequence((&legacy_owner).into(), bound_acl.as_deref()).is_err());
    for value in [
        ValueRef::Null,
        ValueRef::Integer(10),
        ValueRef::Real(10.0),
        ValueRef::Blob(b"uqa"),
        ValueRef::Blob(&[0; 21]),
    ] {
        assert!(decode_sequence(value, None).is_err());
    }
}

#[test]
fn schema_role_storage_class_preserves_names_and_bound_identities_without_guessing() {
    let mut legacy = LegacySchemaRow::legacy("ordinary");
    legacy.role_owner = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for row in [
        SchemaRow::Legacy(legacy),
        SchemaRow::bootstrap("public"),
        SchemaRow::bootstrap("ordinary"),
    ] {
        let (owner, acl) = encode_schema(&row).unwrap();
        assert_eq!(
            matches!(owner, Value::Blob(_)),
            matches!(row, SchemaRow::Bound(_))
        );
        assert_eq!(
            decode_schema(row.name().into(), (&owner).into(), acl.as_deref()).unwrap(),
            row
        );
    }
}

#[test]
fn invalid_role_wire_values_and_mixed_acl_generations_are_rejected() {
    let Value::Blob(original) = encode_identity(RoleIdentity::BOOTSTRAP).unwrap() else {
        unreachable!()
    };
    for bytes in [
        vec![],
        original[..20].to_vec(),
        {
            let mut invalid = original.clone();
            invalid[0] = 2;
            invalid
        },
        {
            let mut invalid = original.clone();
            invalid[1..5].fill(0);
            invalid
        },
        {
            let mut invalid = original.clone();
            invalid[5..].fill(0);
            invalid
        },
    ] {
        assert!(decode_schema("s".into(), ValueRef::Blob(&bytes), None).is_err());
    }
    for owner in [ValueRef::Null, ValueRef::Integer(10), ValueRef::Real(10.0)] {
        assert!(decode_schema("s".into(), owner, None).is_err());
    }
    let (_, named_acl) = encode_schema(&SchemaRow::legacy("public")).unwrap();
    assert!(decode_schema("s".into(), ValueRef::Blob(&original), named_acl.as_deref()).is_err());
    let (_, bound_acl) = encode_schema(&SchemaRow::bootstrap("public")).unwrap();
    assert!(decode_schema("s".into(), ValueRef::Text(b"uqa"), bound_acl.as_deref()).is_err());
    for oid in [-1, 0, i64::from(u32::MAX) + 1] {
        assert!(encode_identity(RoleIdentity {
            oid,
            ..RoleIdentity::BOOTSTRAP
        })
        .is_err());
    }
}

#[test]
fn relation_role_cells_preserve_legacy_names_and_all_bound_acl_endpoints() {
    use uqa_core::{catalog_acl::TablePrivileges, catalog_role::BoundAclEntry};
    let owner = RoleIdentity::BOOTSTRAP;
    let reader = RoleIdentity {
        oid: 20001,
        object_id: [7; 16],
    };
    let mut bound = BoundRelationSecurity::owner(owner);
    let named = BoundAclEntry {
        role: Some(reader),
        grantor: owner,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    };
    let public = BoundAclEntry {
        role: None,
        grantor: reader,
        ..named.clone()
    };
    bound.acl = Some(vec![named]);
    bound.column_acls.insert("value".into(), vec![public]);
    let named_like_json = serde_json::to_string(&owner).unwrap();
    for row in [
        RelationSecurityRow::legacy(named_like_json),
        RelationSecurityRow::Bound(bound),
    ] {
        let (owner, acl, columns) = encode_relation(&row).unwrap();
        assert_eq!(
            matches!(owner, Value::Blob(_)),
            matches!(row, RelationSecurityRow::Bound(_))
        );
        assert_eq!(
            decode_relation((&owner).into(), acl.as_deref(), Some(&columns)).unwrap(),
            row
        );
        assert_eq!(
            decode_relation_cells(
                (&owner).into(),
                super::super::native::optional_text(acl.as_deref()),
                ValueRef::Text(columns.as_bytes())
            )
            .unwrap(),
            row
        );
    }
}

#[test]
fn bound_relation_cells_reject_missing_column_acls_and_mixed_generations() {
    let (owner, _, columns) = encode_relation(&RelationSecurityRow::bootstrap()).unwrap();
    assert!(decode_relation((&owner).into(), None, None).is_err());
    assert!(decode_relation_cells(
        (&owner).into(),
        ValueRef::Integer(0),
        ValueRef::Text(columns.as_bytes())
    )
    .is_err());
    assert!(decode_relation_cells((&owner).into(), ValueRef::Null, ValueRef::Blob(b"{}")).is_err());
    assert!(decode_relation(
        (&owner).into(),
        Some(r#"[{"role":"reader","grantor":"uqa"}]"#),
        Some(&columns)
    )
    .is_err());
    for owner in [
        ValueRef::Null,
        ValueRef::Integer(10),
        ValueRef::Blob(&[1, 2, 3]),
    ] {
        assert!(decode_relation(owner, None, Some("{}")).is_err());
    }
    assert_eq!(
        decode_relation(ValueRef::Text(b"uqa"), None, None).unwrap(),
        RelationSecurityRow::legacy("uqa")
    );
}
