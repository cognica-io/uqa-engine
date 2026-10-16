//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::type_metadata::{builtin_type_name, pg_type_name, pg_type_oid};

fn definition() -> StoredRange {
    StoredRange {
        object_id: [1; 16],
        oid: 36_745,
        array_oid: 36_742,
        array_name: "_r".into(),
        identity: RelationIdentity::new("public", "r"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1_978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [2; 16],
            oid: 36_743,
            array_oid: 36_744,
            array_name: "_r_multirange".into(),
            identity: RelationIdentity::new("public", "r_multirange"),
        },
        usage_acl: None,
    }
}

#[test]
fn declared_range_type_names_preserve_identity_across_independent_namespaces() {
    let mut range = definition();
    let original = range.reference();
    let multi = range.multirange_reference();
    range.identity = RelationIdentity::new("moved", "renamed_range");
    range.multirange.identity = RelationIdentity::new("another", "renamed_multi");
    let restored: StoredRange =
        serde_json::from_str(&serde_json::to_string(&range).unwrap()).unwrap();
    assert_eq!(restored.reference(), original);
    assert_eq!(restored.multirange_reference(), multi);
    for (ty, name, oid) in [
        (restored.column_type(), "renamed_range", range.oid),
        (
            restored.multirange_type(),
            "renamed_multi",
            range.multirange.oid,
        ),
    ] {
        assert_eq!(builtin_type_name(&ty), None);
        assert_eq!(pg_type_name(&ty), name);
        assert_eq!(pg_type_oid(&ty), i64::from(oid));
        assert_eq!(
            pg_type_name(&ColumnType::Array(Box::new(ty))),
            format!("_{name}")
        );
    }
    assert_eq!(restored.reference().schema, "moved");
    assert_eq!(restored.multirange_reference().schema, "another");
    assert_eq!(restored.descriptor().endpoint, ColumnType::Integer);
    assert_eq!(restored.canonical_oid, None);
}

#[test]
fn range_registry_rejects_aliasing_names_oids_and_retired_role_incarnations() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let range = definition();
    validate_range_registry(
        &BTreeMap::from([("public.r".into(), range.clone())]),
        &roles,
    )
    .unwrap();
    for corruption in 0..14 {
        let mut changed = range.clone();
        match corruption {
            0 => changed.object_id = [0; 16],
            1 => changed.multirange.object_id = changed.object_id,
            2 => changed.array_oid = changed.oid,
            3 => changed.multirange.oid = changed.array_oid,
            4 => changed.multirange.array_oid = changed.multirange.oid,
            5 => changed.multirange.array_oid = 0,
            6 => changed.array_name = changed.identity.name.clone(),
            7 => changed.multirange.identity = changed.identity.clone(),
            8 => changed.multirange.array_name = changed.array_name.clone(),
            9 => changed.identity.name = "wrong_key".into(),
            10 => changed.owner.oid += 1,
            11 => {
                changed.subtype = ColumnType::Array(Box::new(ColumnType::Named("missing".into())));
            }
            12 => changed.subtype_operator_class = 0,
            _ => changed.canonical_oid = Some(0),
        }
        assert!(
            validate_range_registry(&BTreeMap::from([("public.r".into(), changed)]), &roles)
                .is_err(),
            "corruption {corruption}"
        );
    }
    for oid in range.type_oids() {
        let mut other = range.clone();
        other.object_id = [3; 16];
        other.multirange.object_id = [4; 16];
        other.identity.schema = "other".into();
        other.multirange.identity.schema = "other".into();
        other.oid = oid;
        other.array_oid = 40_001;
        other.multirange.oid = 40_002;
        other.multirange.array_oid = 40_003;
        assert!(validate_range_registry(
            &BTreeMap::from([
                ("public.r".into(), range.clone()),
                ("other.r".into(), other)
            ]),
            &roles
        )
        .is_err());
    }
}
