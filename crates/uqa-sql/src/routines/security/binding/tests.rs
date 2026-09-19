//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn roles() -> BTreeMap<String, RoleDefinition> {
    ["owner", "delegate", "recipient"]
        .into_iter()
        .zip(1_u8..)
        .map(|(name, id)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_000 + i64::from(id);
            role.object_id = [id; 16];
            role.attributes
                .remove(&crate::ast::RoleAttribute::Superuser);
            (name.into(), role)
        })
        .collect()
}

fn definition(roles: &BTreeMap<String, RoleDefinition>) -> CreateFunction {
    let crate::Statement::CreateFunction(mut definition) =
        crate::compile("CREATE FUNCTION f() RETURNS int RETURN 7")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    definition.owner = Some(roles["owner"].identity());
    definition.execute_acl = Some(vec![
        RoutineAclEntry {
            role: Some(roles["delegate"].identity()),
            grantor: roles["owner"].identity(),
            grant_option: true,
        },
        RoutineAclEntry {
            role: Some(roles["recipient"].identity()),
            grantor: roles["delegate"].identity(),
            grant_option: false,
        },
    ]);
    *definition
}

#[test]
fn routine_owner_and_acl_authority_survives_renames_without_authorizing_replacements() {
    let mut roles = roles();
    let definition = definition(&roles);
    for name in ["owner", "delegate", "recipient"] {
        let mut original = roles.remove(name).unwrap();
        let mut replacement = original.clone();
        replacement.oid += 100;
        replacement.object_id = [original.object_id[0] + 90; 16];
        original.name = format!("renamed_{name}");
        roles.insert(original.name.clone(), original);
        roles.insert(name.into(), replacement);
    }
    validate_routine_authority(&definition, &roles).unwrap();
    assert_eq!(
        routine_role_dependencies(&definition, &roles).unwrap(),
        BTreeSet::from([
            "renamed_owner".into(),
            "renamed_delegate".into(),
            "renamed_recipient".into()
        ])
    );
    let owner = bound_routine_owner(&definition).unwrap();
    for (name, expected) in [("recipient", false), ("renamed_recipient", true)] {
        assert_eq!(
            super::super::routine_privilege_allowed(
                &owner,
                definition.execute_acl.as_deref(),
                false,
                false,
                |identity| *identity == roles[name].identity()
            ),
            expected
        );
    }
    roles.remove("renamed_recipient");
    assert!(validate_routine_authority(&definition, &roles).is_err());
}

#[test]
fn legacy_conversion_preserves_the_two_owner_execute_formats_and_missing_grantors() {
    let roles = roles();
    for implicit in [true, false] {
        let empty = bind_legacy_authority("owner", Some(Vec::new()), &roles, implicit).unwrap();
        assert_eq!(empty.execute_acl.unwrap().len(), usize::from(implicit));
        assert!(bind_legacy_authority("owner", None, &roles, implicit)
            .unwrap()
            .execute_acl
            .is_none());
    }
    let entry: LegacyRoutineAclEntry =
        serde_json::from_value(serde_json::json!({"role":"recipient", "grant_option":false}))
            .unwrap();
    let bound = bind_legacy_authority("owner", Some(vec![entry]), &roles, false).unwrap();
    let entry = &bound.execute_acl.as_ref().unwrap()[0];
    assert_eq!(entry.role, Some(roles["recipient"].identity()));
    assert_eq!(entry.grantor, roles["owner"].identity());
}

#[test]
fn invalid_routine_authority_is_rejected_without_rebinding_matching_oids() {
    let roles = roles();
    for corruption in 0..7 {
        let mut definition = definition(&roles);
        match corruption {
            0 => definition.owner = None,
            1 => definition.owner.as_mut().unwrap().object_id = [99; 16],
            2 => {
                definition.execute_acl.as_mut().unwrap()[0]
                    .grantor
                    .object_id = [99; 16];
            }
            3 => definition.execute_acl.as_mut().unwrap()[0].role = None,
            4 => {
                definition.execute_acl.as_mut().unwrap()[0].grantor = roles["recipient"].identity();
            }
            5 => {
                let acl = definition.execute_acl.as_mut().unwrap();
                acl.push(acl[0].clone());
            }
            _ => {
                definition.execute_acl.as_mut().unwrap()[1]
                    .role
                    .as_mut()
                    .unwrap()
                    .object_id = [99; 16];
            }
        }
        assert!(
            validate_routine_authority(&definition, &roles).is_err(),
            "corruption {corruption}"
        );
    }
}

#[test]
fn added_routine_dependencies_preserve_grantee_and_grantor_incarnations() {
    let roles = roles();
    let after = definition(&roles);
    let mut before = after.clone();
    before.execute_acl = Some(Vec::new());
    let mut added = BTreeSet::new();
    added_routine_acl_roles(&before, &after, &roles, &mut added).unwrap();
    assert_eq!(
        added,
        BTreeSet::from(["delegate".into(), "recipient".into()])
    );
    added.clear();
    added_routine_acl_roles(&after, &after, &roles, &mut added).unwrap();
    assert!(added.is_empty());
    added_routine_acl_roles(&after, &before, &roles, &mut added).unwrap();
    assert!(added.is_empty());
}
