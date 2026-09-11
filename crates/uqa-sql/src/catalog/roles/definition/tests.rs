//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::guards::{RoleDefinitionRead, RoleMembershipRead};
use super::*;
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
    fn current_user_name(&self) -> String {
        let index = self.current_reads.get();
        self.current_reads.set(index + 1);
        format!("current_{index}")
    }
    fn session_user_name(&self) -> String {
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
    creator.attributes = BTreeSet::from([RoleAttribute::CreateRole]);
    inputs.roles.insert("creator".into(), creator);
    let (roles, superuser) = create_role_candidate(&inputs.roles, "creator", &statement).unwrap();
    assert!(!superuser);
    let mut memberships = BTreeMap::new();
    apply_create_role_memberships(
        &inputs.context(),
        &statement,
        "creator",
        superuser,
        &roles,
        &mut memberships,
    )
    .unwrap();
    assert_eq!(memberships.len(), 1);
    let membership = memberships.values().next().unwrap();
    assert_eq!(
        (&*membership.role, &*membership.member, &*membership.grantor),
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
        names: vec!["absent".into(), "SESSION_USER".into()],
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
fn grant_binds_each_live_role_reference_in_granted_grantee_grantor_order() {
    let inputs = Inputs::new();
    let statement = GrantRoleStmt {
        granted_roles: vec!["CURRENT_USER".into()],
        grantee_roles: vec!["CURRENT_USER".into()],
        grantor: Some("CURRENT_USER".into()),
        is_grant: true,
        options: RoleMembershipOptions::default(),
        cascade: false,
    };
    let bound = bind_grant_role_statement(&inputs.context(), &statement);
    assert_eq!(bound.granted_roles, ["current_0"]);
    assert_eq!(bound.grantee_roles, ["current_1"]);
    assert_eq!(bound.grantor.as_deref(), Some("current_2"));
    assert_eq!(inputs.current_reads.get(), 3);
}
