//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;

struct Schemas(SchemaSecurity);
impl RelationOwnerSchemas for Schemas {
    fn schema_security(&self, _: &str) -> Option<SchemaSecurity> {
        Some(self.0.clone())
    }
}

fn roles() -> BTreeMap<String, RoleDefinition> {
    ["actor", "original", "target"]
        .into_iter()
        .map(|name| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.attributes = BTreeSet::from([RoleAttribute::Inherit]);
            (name.into(), role)
        })
        .collect()
}

fn membership(role: &str, inherit: bool, set: bool) -> RoleMembership {
    RoleMembership {
        oid: 20_000,
        role: role.into(),
        member: "actor".into(),
        grantor: "uqa".into(),
        admin_option: false,
        inherit_option: inherit,
        set_option: set,
    }
}

#[test]
fn transfer_requires_existing_owner_privileges_and_separate_target_set_permission() {
    let roles = roles();
    for (inherit, set) in [(false, true), (true, false), (true, true)] {
        let memberships = [
            membership("original", inherit, false),
            membership("target", false, set),
        ]
        .into_iter()
        .map(|membership| (membership.key(), membership))
        .collect();
        let authority = OwnerChangeAuthority {
            roles: &roles,
            memberships: &memberships,
            current_user: &"actor",
            new_owner: "target",
        };
        let result = authority.require_owner_change("original", "table", "items");
        if inherit && set {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("42501"));
            assert!(error
                .to_string()
                .contains(if inherit { "SET ROLE" } else { "must be owner" }));
        }
    }
}

#[test]
fn namespace_create_is_checked_for_the_new_owner_and_bypassed_for_superusers() {
    let mut roles = roles();
    let memberships = BTreeMap::new();
    for schema_owner in ["actor", "target"] {
        let schemas = Schemas(SchemaSecurity {
            role_owner: schema_owner.into(),
            acl: None,
        });
        let authority = OwnerChangeAuthority {
            roles: &roles,
            memberships: &memberships,
            current_user: &"actor",
            new_owner: "target",
        };
        let result = authority.require_schema_create(&schemas, "restricted");
        if schema_owner == "target" {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
        }
    }
    roles
        .get_mut("actor")
        .unwrap()
        .attributes
        .insert(RoleAttribute::Superuser);
    OwnerChangeAuthority {
        roles: &roles,
        memberships: &memberships,
        current_user: &"actor",
        new_owner: "target",
    }
    .require_schema_create(
        &Schemas(SchemaSecurity {
            role_owner: "original".into(),
            acl: Some(Vec::new()),
        }),
        "restricted",
    )
    .unwrap();
}

#[test]
fn schema_owner_transfer_checks_database_create_for_the_invoker() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let authority = OwnerChangeAuthority {
        roles: &roles,
        memberships: &memberships,
        current_user: &"actor",
        new_owner: "target",
    };
    for owner in ["actor", "target"] {
        let security = crate::catalog::security::database::DatabaseSecurity {
            role_owner: owner.into(),
            acl: None,
        };
        let result = authority.require_database_create(&security);
        if owner == "actor" {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
        }
    }
}
