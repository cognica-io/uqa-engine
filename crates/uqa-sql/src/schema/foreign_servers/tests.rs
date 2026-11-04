//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use crate::catalog::{
    foreign_server::ForeignServerMetadata,
    roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        identity::RoleBinding,
        RoleDefinition, RoleMembership, RoleReference,
    },
};
use std::cell::RefCell;

struct Catalog {
    current: RoleReference,
    roles: RefCell<RoleDefinitions>,
    memberships: RefCell<RoleMemberships>,
}

impl Catalog {
    fn new() -> Self {
        let mut owner = RoleDefinition::bootstrap();
        owner.name = "owner".into();
        owner.oid = 20_001;
        owner.object_id = [1; 16];
        owner.attributes.clear();
        let mut member = owner.clone();
        member.name = "member".into();
        member.oid = 20_002;
        member.object_id = [2; 16];
        let bootstrap = RoleDefinition::bootstrap();
        let membership = RoleMembership {
            oid: 20_003,
            role: RoleBinding::from_definition(&owner).unwrap(),
            member: RoleBinding::from_definition(&member).unwrap(),
            grantor: RoleBinding::from_definition(&bootstrap).unwrap(),
            admin_option: false,
            inherit_option: true,
            set_option: true,
        };
        let roles = RoleDefinitions::try_from_iter(
            [owner, member, bootstrap]
                .into_iter()
                .map(|role| (role.name.clone(), role)),
        )
        .unwrap();
        Self {
            current: RoleReference::from_identity(roles["member"].identity(), &roles).unwrap(),
            roles: RefCell::new(roles),
            memberships: RefCell::new(
                RoleMemberships::try_from_iter([(membership.key(), membership)]).unwrap(),
            ),
        }
    }

    fn server(&self) -> ForeignServerDefinition {
        ForeignServerDefinition {
            name: "Mixed.Server".into(),
            fdw_type: "memory_fdw".into(),
            options: BTreeMap::new(),
            metadata: ForeignServerMetadata {
                option_order: None,
                wrapper_reference: None,
                oid: 30_001,
                object_id: [9; 16],
                owner: self.roles.borrow()["owner"].identity(),
                server_type: None,
                version: None,
            },
        }
    }

    fn assert_released(&self) {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
    }
}

impl RoleReferenceNames for Catalog {
    fn current_role(&self) -> RoleReference {
        self.assert_released();
        self.current.clone()
    }
    fn session_role(&self) -> RoleReference {
        panic!("DROP SERVER uses the current role");
    }
    fn outer_role(&self) -> RoleReference {
        panic!("DROP SERVER uses the current role");
    }
}

impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        assert!(self.memberships.try_borrow_mut().is_ok());
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        assert!(self.roles.try_borrow_mut().is_err());
        Box::new(self.memberships.borrow())
    }
}

#[test]
fn server_drop_authority_uses_inheritance_and_releases_catalog_guards() {
    let mut catalog = Catalog::new();
    let server = catalog.server();
    ensure_drop_authority(&server, &catalog, &catalog).unwrap();
    catalog.assert_released();
    let key = *catalog.memberships.borrow().keys().next().unwrap();
    catalog
        .memberships
        .borrow_mut()
        .edit(&key, |membership| {
            membership.inherit_option = false;
            Ok(())
        })
        .unwrap()
        .unwrap();
    let error = ensure_drop_authority(&server, &catalog, &catalog).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(
        error.to_string(),
        "must be owner of foreign server Mixed.Server"
    );
    catalog.assert_released();
    for current in ["owner", "uqa"] {
        catalog.current = current.into();
        ensure_drop_authority(&server, &catalog, &catalog).unwrap();
        catalog.assert_released();
    }
}

#[test]
fn server_drop_authority_follows_owner_rename_but_not_replacement() {
    let catalog = Catalog::new();
    let server = catalog.server();
    let mut owner = catalog.roles.borrow_mut().get("owner").unwrap().clone();
    catalog.roles.borrow_mut().remove("owner").unwrap();
    owner.name = "renamed".into();
    catalog
        .roles
        .borrow_mut()
        .insert(owner.name.clone(), owner)
        .unwrap();
    ensure_drop_authority(&server, &catalog, &catalog).unwrap();
    catalog
        .roles
        .borrow_mut()
        .edit("renamed", |value| {
            value.object_id = [3; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    let error = ensure_drop_authority(&server, &catalog, &catalog).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    catalog.assert_released();
}

#[test]
fn missing_server_error_and_notice_have_distinct_postgresql_states() {
    let error = missing_server("Mixed.Server");
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(error.to_string(), "server \"Mixed.Server\" does not exist");
    let notice = missing_server_notice("Mixed.Server");
    assert_eq!(notice.sqlstate, "00000");
    assert_eq!(
        notice.message,
        "server \"Mixed.Server\" does not exist, skipping"
    );
    assert!(notice.detail.is_none());
    assert!(notice.hint.is_none());
}
