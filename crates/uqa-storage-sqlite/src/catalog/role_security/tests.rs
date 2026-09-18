//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
