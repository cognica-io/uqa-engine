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
        names: vec!["absent".into(), RoleSpecification::SessionUser],
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
