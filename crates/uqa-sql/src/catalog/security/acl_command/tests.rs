//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

struct Names(RoleReference);
impl RoleReferenceNames for Names {
    fn outer_role(&self) -> crate::catalog::roles::RoleReference {
        self.current_role()
    }
    fn current_role(&self) -> RoleReference {
        self.0.clone()
    }
    fn session_role(&self) -> RoleReference {
        self.current_role()
    }
}

fn catalog() -> (BTreeMap<String, RoleDefinition>, Names) {
    let roles: BTreeMap<_, _> = ["grantor", "reader", "PUBLIC"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_000 + i64::try_from(index).unwrap();
            role.object_id = [u8::try_from(index + 1).unwrap(); 16];
            (name.into(), role)
        })
        .collect();
    let names = Names(RoleReference::Bound(Arc::new(
        RoleBinding::from_definition(&roles["grantor"]).unwrap(),
    )));
    (roles, names)
}

#[test]
fn captured_acl_recipients_and_grantor_follow_renames_without_authorizing_reused_names() {
    let (mut roles, names) = catalog();
    let arguments = vec![
        AclRoleSpecification::Public,
        AclRoleSpecification::Role("PUBLIC".into()),
        AclRoleSpecification::Role("reader".into()),
        AclRoleSpecification::Role(RoleSpecification::CurrentUser),
    ];
    let grantor = RoleSpecification::Named("grantor".into());
    let mut command = AclCommandRoles::default();
    command
        .resolve_validated(&names, &roles, &arguments, Some(&grantor), |_| Ok(()))
        .unwrap();
    for old in ["grantor", "reader", "PUBLIC"] {
        let mut original = roles.remove(old).unwrap();
        let mut replacement = original.clone();
        replacement.oid += 10;
        replacement.object_id[0] += 10;
        original.name = format!("renamed {old}");
        original.advance_revision().unwrap();
        roles.insert(original.name.clone(), original);
        roles.insert(old.into(), replacement);
    }
    let resolved = command
        .resolve_validated(&names, &roles, &arguments, Some(&grantor), |_| Ok(()))
        .unwrap();
    assert_eq!(
        resolved.grantees,
        vec![
            AclGrantee::Public,
            AclGrantee::Role("renamed PUBLIC".into()),
            AclGrantee::Role("renamed reader".into()),
            AclGrantee::Role("renamed grantor".into()),
        ]
    );
    assert_eq!(
        resolved.requested_grantor.as_deref(),
        Some("renamed grantor")
    );
    assert_eq!(
        resolved.current_user.bind(&roles).unwrap().identity(),
        names.0.bind(&roles).unwrap().identity()
    );
}

#[test]
fn a_named_actor_is_captured_before_a_wait_can_replace_its_name() {
    let (mut roles, _) = catalog();
    let names = Names(RoleReference::from("grantor"));
    let mut command = AclCommandRoles::default();
    command
        .resolve_validated(&names, &roles, &[], None, |_| Ok(()))
        .unwrap();
    let mut original = roles.remove("grantor").unwrap();
    let identity = original.identity();
    let mut replacement = original.clone();
    replacement.oid += 10;
    replacement.object_id[0] += 10;
    original.name = "renamed".into();
    original.advance_revision().unwrap();
    roles.insert(original.name.clone(), original);
    roles.insert(replacement.name.clone(), replacement);
    let resolved = command
        .resolve_validated(&names, &roles, &[], None, |_| Ok(()))
        .unwrap();
    assert_eq!(
        resolved.current_user.bind(&roles).unwrap().identity(),
        identity
    );
}

#[test]
fn captured_acl_recipients_reject_replacement_incarnations_before_reauthorization() {
    let (mut roles, names) = catalog();
    let arguments = vec![AclRoleSpecification::Role("reader".into())];
    let mut command = AclCommandRoles::default();
    command
        .resolve_validated(&names, &roles, &arguments, None, |_| Ok(()))
        .unwrap();
    roles.get_mut("reader").unwrap().object_id = [9; 16];
    let result = command.resolve_validated(&names, &roles, &arguments, None, |_| {
        panic!("a replacement must not be authorized")
    });
    assert_eq!(result.err().unwrap().sqlstate(), Some("42704"));
}

#[test]
fn object_specific_validation_precedes_binding_and_retains_its_error_order() {
    let (roles, names) = catalog();
    let arguments = vec![AclRoleSpecification::Public];
    let grantor = RoleSpecification::Named("missing".into());
    let mut command = AclCommandRoles::default();
    let result =
        command.resolve_validated(&names, &roles, &arguments, Some(&grantor), |resolved| {
            assert_eq!(resolved.grantees, vec![AclGrantee::Public]);
            assert_eq!(resolved.requested_grantor.as_deref(), Some("missing"));
            Err(SQLError::Routine {
                sqlstate: "0LP01".into(),
                message: "grant options can only be granted to roles".into(),
            })
        });
    assert_eq!(result.err().unwrap().sqlstate(), Some("0LP01"));
    assert!(command.bound.is_none());
}
