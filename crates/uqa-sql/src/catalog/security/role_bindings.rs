//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind catalog role references once and project names from their retained incarnations.

use crate::{
    catalog::roles::{identity::RoleBinding, RoleDefinition, RoleIdentity, RoleReference},
    SQLError,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::Value;

/// PUBLIC and absent role OIDs have no role binding but still receive PUBLIC privileges.
pub(super) fn bind_inquiry_subject(
    value: &Value,
    roles: &BTreeMap<String, RoleDefinition>,
    function: &str,
) -> Result<Option<RoleReference>, SQLError> {
    let role = match value {
        Value::Str(name) | Value::FixedChar(name) if name == "public" => None,
        Value::Str(name) | Value::FixedChar(name) => {
            Some(roles.get(name).ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{name}\" does not exist"),
            })?)
        }
        Value::Int(oid) => roles.values().find(|role| role.oid == *oid),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{function} role must be name or oid, got {other:?}"
            )))
        }
    };
    role.map(|role| {
        RoleBinding::from_definition(role).map(|role| RoleReference::Bound(Arc::new(role)))
    })
    .transpose()
}

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
