//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod fixtures;
mod identity;
use fixtures::{create, Catalog};
use std::collections::BTreeMap;
use uqa_sql::ast::RoleAttribute;

#[test]
fn create_keeps_both_write_guards_and_publishes_only_after_both_persistence_calls() {
    let catalog = Catalog::new();
    let roles = catalog.roles.borrow().clone();
    let mut statement = create("created");
    statement.role_members.push("uqa".into());
    catalog.fail_membership_persistence.set(true);
    assert!(
        matches!(create_role(&catalog.context(), &statement), Err(SQLError::Internal(message)) if message == "membership write failed")
    );
    assert_eq!(*catalog.roles.borrow(), roles);
    assert!(catalog.memberships.borrow().is_empty());
    assert_eq!(catalog.epoch.get(), 0);
    let events = catalog.events.borrow();
    let writer = events.iter().position(|event| event == "writer").unwrap();
    let refreshed = events.iter().rposition(|event| event == "refresh").unwrap();
    assert!(refreshed < writer);
    assert!(events.ends_with(&[
        "write roles".into(),
        "write memberships".into(),
        "persist roles".into(),
        "persist memberships".into(),
        "release memberships".into(),
        "release roles".into(),
    ]));
    drop(events);
    catalog.released();
    catalog.events.borrow_mut().clear();
    catalog.fail_membership_persistence.set(false);
    create_role(&catalog.context(), &statement).unwrap();
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
fn creation_without_memberships_does_not_publish_the_membership_registry() {
    let catalog = Catalog::new();
    catalog.fail_membership_persistence.set(true);
    create_role(&catalog.context(), &create("created")).unwrap();
    assert!(catalog.roles.borrow().contains_key("created"));
    assert!(catalog.memberships.borrow().is_empty());
    assert!(!catalog
        .events
        .borrow()
        .iter()
        .any(|event| event == "persist memberships"));
}

#[test]
fn deletion_publishes_membership_dependencies_even_when_no_visible_edges_are_removed() {
    let catalog = Catalog::new();
    catalog.role("removed", &[]);
    catalog.fail_membership_persistence.set(true);
    let statement = DropRoleStmt {
        names: vec!["removed".into()],
        if_exists: false,
    };
    assert!(
        matches!(drop_roles(&catalog.context(), &statement), Err(SQLError::Internal(message)) if message == "membership write failed")
    );
    assert!(catalog.roles.borrow().contains_key("removed"));
    assert_eq!(catalog.epoch.get(), 0);
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
            "read roles",
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
    let events = catalog.events.borrow();
    let lock = events
        .iter()
        .position(|event| event == "lock role")
        .unwrap();
    let writer = events.iter().position(|event| event == "writer").unwrap();
    assert!(lock < writer);
    assert!(events
        .iter()
        .any(|event| event == "NOTICE: role \"missing\" does not exist, skipping"));
    assert!(!events
        .iter()
        .any(|event| ["database", "schemas", "tables"].contains(&event.as_str())));
    drop(events);
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

#[test]
fn a_non_superuser_admin_cannot_drop_a_superuser_role() {
    let catalog = Catalog::new();
    catalog.role("creator", &[RoleAttribute::CreateRole]);
    catalog.role("privileged", &[RoleAttribute::Superuser]);
    catalog.membership("privileged", "creator", "uqa");
    *catalog.current.borrow_mut() = "creator".into();
    let error = drop_roles(
        &catalog.context(),
        &DropRoleStmt {
            names: vec!["privileged".into()],
            if_exists: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(!catalog
        .events
        .borrow()
        .iter()
        .any(|event| event == "writer" || event == "lock role"));
    assert!(catalog.roles.borrow().contains_key("privileged"));
}

#[test]
fn drop_requires_createrole_before_missing_role_notices_or_current_user_checks() {
    let catalog = Catalog::new();
    catalog.role("limited", &[]);
    *catalog.current.borrow_mut() = "limited".into();
    for name in ["missing", "limited"] {
        catalog.events.borrow_mut().clear();
        let error = drop_roles(
            &catalog.context(),
            &DropRoleStmt {
                names: vec![name.into()],
                if_exists: true,
            },
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert!(!catalog
            .events
            .borrow()
            .iter()
            .any(|event| event.starts_with("NOTICE") || event == "writer" || event == "lock role"));
    }
}
