//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod fixtures;
use fixtures::{create, Catalog};
use std::collections::BTreeMap;
use uqa_sql::ast::RoleAttribute;

#[test]
fn create_keeps_both_write_guards_and_publishes_only_after_both_persistence_calls() {
    let catalog = Catalog::new();
    let roles = catalog.roles.borrow().clone();
    catalog.fail_membership_persistence.set(true);
    assert!(
        matches!(create_role(&catalog.context(), &create("created")), Err(SQLError::Internal(message)) if message == "membership write failed")
    );
    assert_eq!(*catalog.roles.borrow(), roles);
    assert!(catalog.memberships.borrow().is_empty());
    assert_eq!(catalog.epoch.get(), 0);
    assert_eq!(
        *catalog.events.borrow(),
        [
            "current",
            "read roles",
            "release roles",
            "current",
            "read roles",
            "release roles",
            "writer",
            "write roles",
            "write memberships",
            "persist roles",
            "persist memberships",
            "release memberships",
            "release roles"
        ]
    );
    catalog.released();
    catalog.events.borrow_mut().clear();
    catalog.fail_membership_persistence.set(false);
    create_role(&catalog.context(), &create("created")).unwrap();
    assert!(catalog.roles.borrow().contains_key("created"));
    assert_eq!(catalog.epoch.get(), 1);
    assert!(catalog.events.borrow().ends_with(&[
        "persist roles".into(),
        "persist memberships".into(),
        "publish roles".into(),
        "publish memberships".into(),
        "release memberships".into(),
        "release roles".into(),
        "epoch".into()
    ]));
}

#[test]
fn duplicate_creation_stops_before_membership_write_or_persistence() {
    let catalog = Catalog::new();
    let error = create_role(&catalog.context(), &create("uqa")).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "42710"));
    assert_eq!(
        *catalog.events.borrow(),
        [
            "current",
            "read roles",
            "release roles",
            "current",
            "read roles",
            "release roles",
            "writer",
            "write roles",
            "release roles"
        ]
    );
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn alter_holds_role_write_while_checking_membership_administration() {
    let catalog = Catalog::new();
    catalog.role("creator", &[RoleAttribute::CreateRole]);
    catalog.role("managed", &[]);
    catalog.membership("managed", "creator", "uqa");
    *catalog.current.borrow_mut() = "creator".into();
    let statement = AlterRoleStmt {
        name: "managed".into(),
        attributes: BTreeMap::from([(RoleAttribute::Login, true)]),
        connection_limit: Some(3),
        membership_action: None,
        members: Vec::new(),
    };
    alter_role(&catalog.context(), &statement).unwrap();
    assert_eq!(
        *catalog.events.borrow(),
        [
            "current",
            "writer",
            "write roles",
            "read memberships",
            "release memberships",
            "persist roles",
            "publish roles",
            "release roles",
            "epoch"
        ]
    );
    let roles = catalog.roles.borrow();
    assert!(roles["managed"].has(RoleAttribute::Login));
    assert_eq!(roles["managed"].connection_limit, 3);
}

#[test]
fn grant_prepares_writer_before_binding_names_and_retains_authorization_through_publication() {
    let catalog = Catalog::new();
    catalog.role("team", &[]);
    let statement = GrantRoleStmt {
        granted_roles: vec!["team".into()],
        grantee_roles: vec!["CURRENT_USER".into()],
        is_grant: true,
        options: RoleMembershipOptions::default(),
        grantor: None,
        cascade: false,
    };
    grant_roles(&catalog.context(), &statement).unwrap();
    assert_eq!(
        *catalog.events.borrow(),
        [
            "writer",
            "current",
            "read roles",
            "current",
            "write memberships",
            "persist memberships",
            "publish memberships",
            "release memberships",
            "release roles",
            "epoch"
        ]
    );
    assert_eq!(catalog.memberships.borrow().len(), 1);
}

#[test]
fn drop_reports_grantor_dependency_before_object_catalog_reads() {
    let catalog = Catalog::new();
    for name in ["grantor", "team", "member"] {
        catalog.role(name, &[]);
    }
    catalog.membership("team", "member", "grantor");
    let roles = catalog.roles.borrow().clone();
    let statement = DropRoleStmt {
        names: vec!["missing".into(), "grantor".into()],
        if_exists: true,
    };
    let error = drop_roles(&catalog.context(), &statement).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "2BP01" && message == "role \"grantor\" cannot be dropped because some objects depend on it: privileges for membership of role member in role team")
    );
    assert_eq!(
        *catalog.events.borrow(),
        [
            "current",
            "session",
            "writer",
            "write roles",
            "NOTICE: role \"missing\" does not exist, skipping",
            "write memberships",
            "release memberships",
            "release roles"
        ]
    );
    assert_eq!(*catalog.roles.borrow(), roles);
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn set_role_releases_authorization_guards_before_changing_current_identity() {
    let catalog = Catalog::new();
    catalog.role("reduced", &[]);
    catalog.role("target", &[]);
    *catalog.current.borrow_mut() = "reduced".into();
    set_role(&catalog.context(), "target").unwrap();
    assert_eq!(*catalog.current.borrow(), "target");
    assert_eq!(
        *catalog.events.borrow(),
        [
            "read roles",
            "session",
            "read memberships",
            "release memberships",
            "release roles",
            "set current"
        ]
    );
    catalog.events.borrow_mut().clear();
    assert!(set_role(&catalog.context(), "missing").is_err());
    assert_eq!(*catalog.current.borrow(), "target");
    assert_eq!(*catalog.events.borrow(), ["read roles", "release roles"]);
}
