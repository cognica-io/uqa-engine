//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;

#[test]
fn relation_ownership_errors_name_the_actual_kind() {
    for kind in [
        "table",
        "view",
        "materialized view",
        "foreign table",
        "index",
        "sequence",
    ] {
        require_relation_ownership("target", kind, true).unwrap();
        let error = require_relation_ownership("target", kind, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert!(error
            .to_string()
            .contains(&format!("must be owner of {kind} target")));
    }
}

#[test]
fn relation_alteration_protects_pinned_catalogs_but_not_unpinned_views() {
    for (schema, name, protected) in [
        ("pg_catalog", "pg_class", true),
        ("pg_catalog", "pg_authid", true),
        ("pg_catalog", "pg_shadow", false),
        ("information_schema", "enabled_roles", false),
        ("public", "pg_class", false),
        ("pg_toast", "pg_toast_12345", true),
    ] {
        let result = reject_system_relation_alter(&uqa_core::RelationIdentity::new(schema, name));
        if protected {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("42501"));
            assert!(error.to_string().contains("is a system catalog"));
        } else {
            result.unwrap();
        }
    }
}

struct Schemas(BoundSchemaSecurity);
impl RelationOwnerSchemas for Schemas {
    fn schema_security(&self, _: &str) -> Option<BoundSchemaSecurity> {
        Some(self.0.clone())
    }
}

fn roles() -> BTreeMap<String, RoleDefinition> {
    ["actor", "original", "target"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_001 + index as i64;
            role.object_id = [index as u8 + 1; 16];
            role.attributes = BTreeSet::from([RoleAttribute::Inherit]);
            (name.into(), role)
        })
        .collect()
}

fn membership(role: &str, inherit: bool, set: bool) -> RoleMembership {
    use crate::catalog::roles::identity::RoleBinding;
    let roles = roles();
    RoleMembership {
        oid: 20_000,
        role: RoleBinding::from_definition(&roles[role]).unwrap(),
        member: RoleBinding::from_definition(&roles["actor"]).unwrap(),
        grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
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
        let schemas = Schemas(BoundSchemaSecurity {
            tuple: None,
            role_owner: roles[schema_owner].identity(),
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
        &Schemas(BoundSchemaSecurity {
            tuple: None,
            role_owner: roles["original"].identity(),
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
        let bound =
            crate::catalog::security::database::BoundDatabaseSecurity::bind(&security, &roles)
                .unwrap();
        let result = authority.require_database_create(&bound);
        if owner == "actor" {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
        }
    }
}
