//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::security::{grant_routine_acl, revoke_routine_acl};

struct Catalog {
    acl: Option<Vec<RoutineAclEntry>>,
}
impl RoutinePrivilegeCatalog for Catalog {
    fn resolve_routine_name(&self, name: &str) -> Result<i64, SQLError> {
        if name == "f()" {
            Ok(1)
        } else if name == "99" {
            Ok(99)
        } else {
            Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: "missing function".into(),
            })
        }
    }
    fn routine_privileges(&self, oid: i64) -> Result<Option<RoutinePrivileges<'_>>, SQLError> {
        Ok((oid == 1).then_some(RoutinePrivileges {
            owner: "owner",
            execute_acl: self.acl.as_deref(),
        }))
    }
}

fn roles() -> BTreeMap<String, RoleDefinition> {
    let mut owner = RoleDefinition::bootstrap();
    owner.name = "owner".into();
    owner.oid = 11;
    owner.object_id = [1; 16];
    owner.attributes.remove(&RoleAttribute::Superuser);
    let mut reader = owner.clone();
    reader.name = "reader".into();
    reader.oid = 12;
    reader.object_id = [2; 16];
    [RoleDefinition::bootstrap(), owner, reader]
        .into_iter()
        .map(|role| (role.name.clone(), role))
        .collect()
}

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

#[test]
fn function_inquiry_distinguishes_public_unknown_roles_and_missing_target_input_types() {
    let roles = roles();
    let memberships = BTreeMap::new();
    for acl in [None, Some(Vec::new())] {
        let public = acl.is_none();
        let catalog = Catalog { acl };
        let inquiry = RoutinePrivilegeInquiry {
            current_user: &"reader".into(),
            roles: &roles,
            memberships: &memberships,
            catalog: &catalog,
        };
        for subject in [text("public"), Value::Int(999)] {
            assert_eq!(
                inquiry
                    .has_function_privilege_value(&[subject.clone(), text("f()"), text("EXECUTE")])
                    .unwrap(),
                Value::Bool(public)
            );
            assert_eq!(
                inquiry
                    .has_function_privilege_value(&[
                        subject,
                        text("f()"),
                        text("EXECUTE WITH GRANT OPTION")
                    ])
                    .unwrap(),
                Value::Bool(false)
            );
        }
        assert_eq!(
            inquiry
                .has_function_privilege_value(&[Value::Int(99), text("EXECUTE")])
                .unwrap(),
            Value::Null
        );
        assert_eq!(
            inquiry
                .has_function_privilege_value(&[text("99"), text("EXECUTE")])
                .unwrap_err()
                .sqlstate(),
            Some("XX000")
        );
        for target in [text("99"), Value::Int(99)] {
            assert_eq!(
                inquiry
                    .has_function_privilege_value(&[text("uqa"), target, text("EXECUTE")])
                    .unwrap(),
                Value::Bool(true)
            );
        }
        for privilege in ["EXECUTE", "EXECUTE WITH GRANT OPTION"] {
            assert_eq!(
                inquiry
                    .has_function_privilege_value(&[text("owner"), text("f()"), text(privilege)])
                    .unwrap(),
                Value::Bool(public || privilege.ends_with("OPTION"))
            );
        }
    }
}

#[test]
fn function_inquiry_preserves_strictness_error_order_and_exact_privilege_tokens() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let catalog = Catalog { acl: None };
    let inquiry = RoutinePrivilegeInquiry {
        current_user: &"reader".into(),
        roles: &roles,
        memberships: &memberships,
        catalog: &catalog,
    };
    assert_eq!(
        inquiry
            .has_function_privilege_value(&[text("missing"), Value::Null, text("INVALID")])
            .unwrap(),
        Value::Null
    );
    for (arguments, state) in [
        (
            vec![text("missing"), text("missing()"), text("INVALID")],
            "42704",
        ),
        (vec![text("PUBLIC"), text("f()"), text("EXECUTE")], "42704"),
        (vec![text("missing()"), text("INVALID")], "42883"),
        (vec![Value::Int(99), text("INVALID")], "22023"),
    ] {
        assert_eq!(
            inquiry
                .has_function_privilege_value(&arguments)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
    }
    for invalid in [
        "",
        "SELECT",
        "EXECUTE  WITH GRANT OPTION",
        "EXECUTE,",
        ",EXECUTE",
    ] {
        assert_eq!(
            parse_privileges(invalid).unwrap_err().sqlstate(),
            Some("22023")
        );
    }
    assert_eq!(
        parse_privileges(" execute with grant option,\tEXECUTE ").unwrap(),
        [true, false]
    );
}

#[test]
fn revoking_owner_execute_preserves_grant_authority_and_delegated_privileges() {
    let crate::Statement::CreateFunction(mut definition) =
        crate::compile("CREATE FUNCTION f() RETURNS integer RETURN 1")
            .unwrap()
            .remove(0)
    else {
        panic!("routine definition");
    };
    definition.owner = "owner".into();
    grant_routine_acl(&mut definition, &"reader".into(), "owner", true);
    revoke_routine_acl(
        &mut definition,
        &uqa_core::catalog_acl::AclGrantee::Public,
        "owner",
        false,
        false,
    )
    .unwrap();
    revoke_routine_acl(&mut definition, &"owner".into(), "owner", false, false).unwrap();
    let has = |subject: &str, option| {
        routine_privilege_allowed(
            &definition.owner,
            definition.execute_acl.as_deref(),
            option,
            false,
            |role| role == subject,
        )
    };
    assert!(!has("owner", false));
    assert!(has("owner", true));
    assert!(has("reader", true));
    grant_routine_acl(&mut definition, &"owner".into(), "owner", false);
    assert!(routine_privilege_allowed(
        &definition.owner,
        definition.execute_acl.as_deref(),
        false,
        false,
        |role| role == "owner"
    ));
}
