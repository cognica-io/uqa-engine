//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn unknown_or_malformed_routine_formats_are_not_treated_as_legacy() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for json in [
        r#"{"routine_catalog_format":0,"definitions":{}}"#,
        r#"{"routine_catalog_format":3,"definitions":{}}"#,
        r#"{"routine_catalog_format":2}"#,
        r#"{"routine_catalog_format":null,"definitions":{}}"#,
    ] {
        assert!(decode(Some(json), &roles, true).is_err(), "{json}");
    }
    assert!(decode(Some("{}"), &roles, true).unwrap().1);
    assert!(decode(Some("{}"), &roles, false).is_err());
    assert!(
        !decode(Some(&encode(BTreeMap::new()).unwrap()), &roles, false)
            .unwrap()
            .1
    );
}

fn fixture() -> (Definitions, BTreeMap<String, RoleDefinition>) {
    let mut owner = RoleDefinition::bootstrap();
    owner.name = "owner".into();
    owner.oid = 20_001;
    owner.object_id = [7; 16];
    let uqa_sql::Statement::CreateFunction(mut definition) =
        uqa_sql::compile("CREATE FUNCTION public.f() RETURNS int RETURN 7")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    definition.owner = Some(owner.identity());
    definition.object_id = Some([2; 16]);
    definition.execute_acl = Some(Vec::new());
    (
        BTreeMap::from([("public.f".into(), vec![*definition])]),
        BTreeMap::from([("owner".into(), owner)]),
    )
}

#[test]
fn legacy_formats_preserve_revoked_owner_execute_and_default_public_execution() {
    let (definitions, roles) = fixture();
    for format in [0, 1] {
        for explicit in [false, true] {
            let mut legacy = serde_json::to_value(&definitions).unwrap();
            let definition = &mut legacy["public.f"][0];
            definition["owner"] = "owner".into();
            if !explicit {
                definition.as_object_mut().unwrap().remove("execute_acl");
            }
            let json = if format == 0 {
                legacy
            } else {
                serde_json::json!({"routine_catalog_format":1, "definitions":legacy})
            }
            .to_string();
            assert!(decode(Some(&json), &roles, false).is_err());
            let (decoded, migrated) = decode(Some(&json), &roles, true).unwrap();
            assert!(migrated);
            let definition = &decoded["public.f"][0];
            assert_eq!(definition.owner, Some(roles["owner"].identity()));
            assert_eq!(definition.object_id, Some([2; 16]));
            if explicit {
                assert_eq!(
                    definition.execute_acl.as_ref().unwrap().len(),
                    usize::from(format == 0)
                );
            } else {
                assert!(definition.execute_acl.is_none());
            }
            assert!(
                !decode(Some(&encode(decoded).unwrap()), &roles, false)
                    .unwrap()
                    .1
            );
        }
    }
}

#[test]
fn current_routine_authority_follows_renames_and_rejects_replacement_incarnations() {
    let (definitions, mut roles) = fixture();
    let identity = roles["owner"].identity();
    let json = encode(definitions).unwrap();
    let mut original = roles.remove("owner").unwrap();
    let mut replacement = original.clone();
    replacement.oid += 1;
    replacement.object_id = [8; 16];
    original.name = "renamed".into();
    roles.insert("renamed".into(), original);
    roles.insert("owner".into(), replacement);
    assert_eq!(
        decode(Some(&json), &roles, false).unwrap().0["public.f"][0].owner,
        Some(identity)
    );
    roles.remove("renamed");
    roles.get_mut("owner").unwrap().oid = identity.oid;
    for migrate in [false, true] {
        assert!(decode(Some(&json), &roles, migrate).is_err());
    }
}

#[test]
fn malformed_current_authority_never_falls_back_to_a_name_or_default_acl() {
    let (mut definitions, roles) = fixture();
    let identity = roles["owner"].identity();
    definitions.get_mut("public.f").unwrap()[0].execute_acl =
        Some(vec![uqa_sql::ast::RoutineAclEntry {
            role: Some(identity),
            grantor: identity,
            grant_option: false,
        }]);
    let original: serde_json::Value = serde_json::from_str(&encode(definitions).unwrap()).unwrap();
    for corruption in 0..9 {
        let mut value = original.clone();
        let definition = &mut value["definitions"]["public.f"][0];
        match corruption {
            0 => {
                definition.as_object_mut().unwrap().remove("owner");
            }
            1 => definition["owner"] = "owner".into(),
            2 => definition["owner"] = "".into(),
            3 => {
                definition.as_object_mut().unwrap().remove("execute_acl");
            }
            4 => {
                definition["execute_acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("role");
            }
            5 => {
                definition["execute_acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("grantor");
            }
            6 => definition["owner"]["object_id"] = serde_json::to_value([9_u8; 16]).unwrap(),
            7 => {
                definition["execute_acl"][0]["grantor"]["object_id"] =
                    serde_json::to_value([9_u8; 16]).unwrap();
            }
            _ => {
                definition.as_object_mut().unwrap().remove("object_id");
            }
        }
        for migrate in [false, true] {
            assert!(
                decode(Some(&value.to_string()), &roles, migrate).is_err(),
                "corruption {corruption}, initial {migrate}"
            );
        }
    }
}
