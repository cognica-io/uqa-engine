//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::guards::{RoleDefinitionRead, RoleMembershipRead};
use super::*;
use crate::ast::{CreateRoleStmt, GrantRoleStmt, RoleMembershipOptions, RoleSpecification};
use crate::catalog::roles::{
    memberships::command::{creator_membership, MembershipRecipients},
    RoleReference,
};
use std::cell::{Cell, RefCell};

struct Inputs {
    roles: BTreeMap<String, RoleDefinition>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    notices: RefCell<Vec<(String, String)>>,
    current_reads: Cell<usize>,
}
impl Inputs {
    fn new() -> Self {
        Self {
            roles: BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
            memberships: BTreeMap::new(),
            notices: RefCell::new(Vec::new()),
            current_reads: Cell::new(0),
        }
    }
    fn context(&self) -> RoleValidationContext<'_> {
        RoleValidationContext {
            names: self,
            roles: self,
            notices: self,
        }
    }
}
impl RoleReferenceNames for Inputs {
    fn outer_role(&self) -> crate::catalog::roles::RoleReference {
        self.current_role()
    }
    fn current_role(&self) -> RoleReference {
        let index = self.current_reads.get();
        self.current_reads.set(index + 1);
        format!("current_{index}").into()
    }
    fn session_role(&self) -> RoleReference {
        "uqa".into()
    }
}
impl RoleCatalogGuards for Inputs {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}
impl RoleNotices for Inputs {
    fn notice(&self, level: &str, message: &str) {
        self.notices
            .borrow_mut()
            .push((level.into(), message.into()));
    }
}

#[test]
fn role_creation_grants_creator_administration_without_inherit_or_set() {
    let mut inputs = Inputs::new();
    let statement = CreateRoleStmt {
        name: "created".into(),
        attributes: BTreeSet::from([RoleAttribute::Inherit]),
        connection_limit: -1,
        in_roles: Vec::new(),
        role_members: Vec::new(),
        admin_members: Vec::new(),
    };
    let mut creator = RoleDefinition::bootstrap();
    creator.name = "creator".into();
    creator.oid = 20_002;
    creator.object_id = [2; 16];
    creator.attributes = BTreeSet::from([RoleAttribute::CreateRole]);
    inputs.roles.insert("creator".into(), creator);
    let mut other_superuser = RoleDefinition::bootstrap();
    other_superuser.name = "another_superuser".into();
    other_superuser.oid = 20_003;
    other_superuser.object_id = [3; 16];
    inputs
        .roles
        .insert(other_superuser.name.clone(), other_superuser);
    let (roles, superuser) = create_role_candidate(
        &inputs.roles,
        "creator",
        RoleDefinition::from_create(&statement, 20_001, [1; 16]),
    )
    .unwrap();
    assert!(!superuser);
    let membership = creator_membership(&roles, "creator", &roles["created"])
        .unwrap()
        .with_oid(31_000)
        .unwrap();
    assert_eq!(
        (
            membership.role.name.as_str(),
            membership.member.name.as_str(),
            membership.grantor.name.as_str()
        ),
        ("created", "creator", "uqa")
    );
    assert!(membership.admin_option);
    assert!(!membership.inherit_option);
    assert!(!membership.set_option);
    assert!(!inputs.roles.contains_key("created"));
}

#[test]
fn drop_missing_role_notice_precedes_a_later_current_user_error() {
    let inputs = Inputs::new();
    let statement = DropRoleStmt {
        names: vec!["absent".into(), "uqa".into()],
        if_exists: true,
    };
    let error = resolve_drop_role_names(&inputs.context(), &statement, "uqa", "uqa", &inputs.roles)
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "55006" && message == "current user cannot be dropped")
    );
    assert_eq!(
        *inputs.notices.borrow(),
        [(
            "NOTICE".into(),
            "role \"absent\" does not exist, skipping".into()
        )]
    );
}

#[test]
fn grant_binds_explicit_grantor_before_recipients_without_aliasing_target_names() {
    let mut inputs = Inputs::new();
    for index in 0..2 {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("current_{index}");
        role.oid = 20_001 + index;
        role.object_id = [index as u8 + 1; 16];
        inputs.roles.insert(role.name.clone(), role);
    }
    let statement = GrantRoleStmt {
        granted_roles: vec!["CURRENT_USER".into()],
        grantee_roles: vec![RoleSpecification::CurrentUser],
        grantor: Some(RoleSpecification::CurrentUser),
        is_grant: true,
        options: RoleMembershipOptions::default(),
        cascade: false,
    };
    let bound = MembershipRecipients::bind(&inputs, &inputs.roles, &statement).unwrap();
    assert_eq!(bound.grantor.unwrap().name, "current_0");
    assert_eq!(bound.members[0].name, "current_1");
    assert_eq!(inputs.current_reads.get(), 2);
}

#[test]
fn drop_special_role_targets_check_createrole_before_rejecting_specifications() {
    let mut inputs = Inputs::new();
    for (index, name) in ["plain", "creator"].into_iter().enumerate() {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_000 + i64::try_from(index).unwrap();
        role.object_id = [u8::try_from(index + 1).unwrap(); 16];
        role.attributes = if name == "creator" {
            BTreeSet::from([RoleAttribute::CreateRole])
        } else {
            BTreeSet::new()
        };
        inputs.roles.insert(name.into(), role);
    }
    for current in ["plain", "creator", "uqa"] {
        for requested in [
            RoleSpecification::CurrentUser,
            RoleSpecification::SessionUser,
            RoleSpecification::Named("public".into()),
        ] {
            for if_exists in [false, true] {
                let statement = DropRoleStmt {
                    names: vec![requested.clone()],
                    if_exists,
                };
                let error = resolve_drop_role_names(
                    &inputs.context(),
                    &statement,
                    current,
                    "uqa",
                    &inputs.roles,
                )
                .unwrap_err();
                assert_eq!(
                    error.sqlstate(),
                    Some(if current == "plain" { "42501" } else { "22023" }),
                    "{current}/{requested}/{if_exists}"
                );
                if current != "plain" {
                    assert!(
                        matches!(error, SQLError::Routine { message, .. } if message == "cannot use special role specifier in DROP ROLE")
                    );
                }
            }
        }
    }
    assert_eq!(inputs.current_reads.get(), 0);
    assert!(inputs.notices.borrow().is_empty());
}

#[test]
fn drop_special_role_targets_preserve_prior_missing_name_errors_and_notices() {
    for if_exists in [false, true] {
        let inputs = Inputs::new();
        let statement = DropRoleStmt {
            names: vec!["absent".into(), RoleSpecification::CurrentUser],
            if_exists,
        };
        let error =
            resolve_drop_role_names(&inputs.context(), &statement, "uqa", "uqa", &inputs.roles)
                .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if if_exists { "22023" } else { "42704" })
        );
        assert_eq!(inputs.current_reads.get(), 0);
        let expected = if if_exists {
            vec![(
                "NOTICE".into(),
                "role \"absent\" does not exist, skipping".into(),
            )]
        } else {
            Vec::new()
        };
        assert_eq!(*inputs.notices.borrow(), expected);
    }
}

#[test]
fn drop_special_role_targets_leave_quoted_uppercase_names_literal() {
    let mut inputs = Inputs::new();
    let names = ["CURRENT_USER", "CURRENT_ROLE", "SESSION_USER", "PUBLIC"];
    for (index, name) in names.into_iter().enumerate() {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_000 + i64::try_from(index).unwrap();
        role.object_id = [u8::try_from(index + 1).unwrap(); 16];
        inputs.roles.insert(name.into(), role);
    }
    let statement = DropRoleStmt {
        names: names.map(RoleSpecification::from).to_vec(),
        if_exists: false,
    };
    assert_eq!(
        resolve_drop_role_names(&inputs.context(), &statement, "uqa", "uqa", &inputs.roles)
            .unwrap(),
        names.map(str::to_owned)
    );
}

#[test]
fn role_deletion_protects_effective_outer_and_session_incarnations() {
    struct Names {
        effective: RoleReference,
        outer: RoleReference,
        session: RoleReference,
    }
    impl RoleReferenceNames for Names {
        fn current_role(&self) -> RoleReference {
            self.effective.clone()
        }
        fn session_role(&self) -> RoleReference {
            self.session.clone()
        }
        fn outer_role(&self) -> RoleReference {
            self.outer.clone()
        }
    }
    let mut inputs = Inputs::new();
    for (index, name) in ["effective", "outer"].into_iter().enumerate() {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_000 + i64::try_from(index).unwrap();
        role.object_id = [u8::try_from(index + 1).unwrap(); 16];
        inputs.roles.insert(name.into(), role);
    }
    let reference = |name: &str| {
        RoleReference::from_identity(inputs.roles[name].identity(), &inputs.roles).unwrap()
    };
    let names = Names {
        effective: reference("effective"),
        outer: reference("outer"),
        session: reference("uqa"),
    };
    let check = |inputs: &Inputs, target: &str| {
        require_role_drop_authority(
            &RoleValidationContext {
                names: &names,
                roles: inputs,
                notices: inputs,
            },
            &inputs.roles,
            &names.effective,
            &names.session,
            target,
        )
    };
    for (target, message) in [
        ("effective", "current user cannot be dropped"),
        ("outer", "current user cannot be dropped"),
        ("uqa", "session user cannot be dropped"),
    ] {
        assert!(
            matches!(check(&inputs, target), Err(SQLError::Routine { sqlstate, message: actual }) if sqlstate == "55006" && actual == message),
            "{target}"
        );
    }
    let mut original = inputs.roles.remove("outer").unwrap();
    let mut replacement = original.clone();
    replacement.oid += 10;
    replacement.object_id[0] += 10;
    original.name = "renamed_outer".into();
    original.advance_revision().unwrap();
    inputs.roles.insert(original.name.clone(), original);
    inputs.roles.insert(replacement.name.clone(), replacement);
    assert_eq!(
        check(&inputs, "renamed_outer").unwrap_err().sqlstate(),
        Some("55006")
    );
    check(&inputs, "outer").unwrap();
}
