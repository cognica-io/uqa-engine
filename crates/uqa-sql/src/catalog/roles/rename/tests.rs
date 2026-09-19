//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RoleAttribute,
    catalog::roles::{
        definition::RoleNotices,
        guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
        identity::RoleBinding,
        RoleMembership, RoleMembershipKey, RoleReference, RoleReferenceNames,
    },
};
use std::collections::BTreeSet;

struct Inputs {
    roles: BTreeMap<String, RoleDefinition>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &'static str,
    session: &'static str,
    outer: &'static str,
}

impl Inputs {
    fn new() -> Self {
        let mut roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
        for (index, name) in ["target", "actor", "plain", "existing", "pg_reserved"]
            .into_iter()
            .enumerate()
        {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_000 + i64::try_from(index).unwrap();
            role.object_id = [u8::try_from(index + 1).unwrap(); 16];
            role.attributes = BTreeSet::from([RoleAttribute::Inherit, RoleAttribute::Login]);
            role.connection_limit = 7;
            roles.insert(role.name.clone(), role);
        }
        Self {
            roles,
            memberships: BTreeMap::new(),
            current: "plain",
            session: "uqa",
            outer: "plain",
        }
    }

    fn candidate(&self, from: &str, to: &str) -> Result<RoleDefinition, SQLError> {
        rename_candidate(
            &RoleValidationContext {
                names: self,
                roles: self,
                notices: self,
            },
            &self.roles,
            &RenameRoleStmt {
                name: from.into(),
                new_name: to.into(),
            },
        )
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

impl RoleReferenceNames for Inputs {
    fn current_role(&self) -> RoleReference {
        self.current.into()
    }
    fn session_role(&self) -> RoleReference {
        self.session.into()
    }
    fn outer_role(&self) -> RoleReference {
        self.outer.into()
    }
}

impl RoleNotices for Inputs {
    fn notice(&self, _: &str, _: &str) {
        panic!("role rename has no notices without password state");
    }
}

#[test]
fn rename_validates_target_session_reserved_names_and_duplicates_before_authority() {
    let inputs = Inputs::new();
    for (from, to, state) in [
        ("missing", "existing", "42704"),
        ("uqa", "existing", "0A000"),
        ("plain", "existing", "0A000"),
        ("pg_reserved", "existing", "42939"),
        ("target", "pg_reserved", "42939"),
        ("target", "target", "42710"),
        ("target", "existing", "42710"),
        ("target", "renamed", "42501"),
    ] {
        assert_eq!(
            inputs.candidate(from, to).unwrap_err().sqlstate(),
            Some(state),
            "{from} -> {to}"
        );
    }
    assert_eq!(
        require_available_name(&inputs.roles, "existing", true)
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
}

#[test]
fn rename_requires_createrole_and_admin_for_non_superuser_targets() {
    for create in [false, true] {
        for admin in [false, true] {
            for superuser_target in [false, true] {
                let mut inputs = Inputs::new();
                inputs.current = "actor";
                inputs.outer = "actor";
                if create {
                    inputs
                        .roles
                        .get_mut("actor")
                        .unwrap()
                        .attributes
                        .insert(RoleAttribute::CreateRole);
                }
                if superuser_target {
                    inputs
                        .roles
                        .get_mut("target")
                        .unwrap()
                        .attributes
                        .insert(RoleAttribute::Superuser);
                }
                let membership = RoleMembership {
                    oid: 30_000,
                    role: RoleBinding::from_definition(&inputs.roles["target"]).unwrap(),
                    member: RoleBinding::from_definition(&inputs.roles["actor"]).unwrap(),
                    grantor: RoleBinding::from_definition(&inputs.roles["uqa"]).unwrap(),
                    admin_option: admin,
                    inherit_option: true,
                    set_option: true,
                };
                inputs.memberships.insert(membership.key(), membership);
                let result = inputs.candidate("target", "renamed");
                if create && admin && !superuser_target {
                    result.unwrap();
                } else {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
                }
            }
        }
    }
}

#[test]
fn rename_preserves_identity_attributes_and_original_catalog() {
    let mut inputs = Inputs::new();
    inputs.current = "uqa";
    let before = inputs.roles.clone();
    let renamed = inputs.candidate("target", "CURRENT_USER").unwrap();
    assert_eq!(renamed.identity(), before["target"].identity());
    assert_eq!(renamed.attributes, before["target"].attributes);
    assert_eq!(renamed.connection_limit, 7);
    assert_eq!(renamed.revision, before["target"].revision + 1);
    assert_eq!(renamed.name, "CURRENT_USER");
    assert_eq!(inputs.roles, before);
}

#[test]
fn definer_authority_does_not_replace_the_outer_user_rename_restriction() {
    let mut inputs = Inputs::new();
    inputs.current = "target";
    inputs.outer = "actor";
    inputs
        .roles
        .get_mut("target")
        .unwrap()
        .attributes
        .insert(RoleAttribute::Superuser);
    inputs.candidate("target", "renamed").unwrap();
    assert_eq!(
        inputs.candidate("actor", "renamed").unwrap_err().sqlstate(),
        Some("0A000")
    );
}
