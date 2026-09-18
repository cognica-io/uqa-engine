//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind catalog role references once and project names from their retained incarnations.

use crate::catalog::roles::{identity::RoleBinding, RoleDefinition, RoleIdentity};
use std::collections::BTreeMap;

pub(super) fn bind_role(
    roles: &BTreeMap<String, RoleDefinition>,
    name: &str,
    catalog: &str,
) -> Result<RoleIdentity, String> {
    let role = roles
        .get(name)
        .ok_or_else(|| format!("persisted {catalog} privileges reference missing role `{name}`"))?;
    RoleBinding::from_definition(role)
        .map(|binding| binding.identity())
        .map_err(|error| error.to_string())
}

pub(super) fn role_name<'a>(
    roles: &'a BTreeMap<String, RoleDefinition>,
    identity: RoleIdentity,
    catalog: &str,
) -> Result<&'a str, String> {
    if !identity.is_valid() {
        return Err(format!("invalid persisted {catalog} role identity"));
    }
    roles
        .values()
        .find(|role| role.identity() == identity)
        .map(|role| role.name.as_str())
        .ok_or_else(|| {
            format!(
                "persisted {catalog} privileges reference missing role incarnation {}",
                identity.oid
            )
        })
}
