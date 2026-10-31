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
    role_definition_with_control(
        roles,
        identity,
        catalog,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|role| role.name.as_str())
    .map_err(|error| match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    })
}

pub(super) fn role_definition_with_control<'a>(
    roles: &'a BTreeMap<String, RoleDefinition>,
    identity: RoleIdentity,
    catalog: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<&'a RoleDefinition, SQLError> {
    use crate::catalog::roles::identity::RoleSubject;
    control.check()?;
    if !identity.is_valid() {
        return Err(SQLError::Internal(format!(
            "invalid persisted {catalog} role identity"
        )));
    }
    identity
        .role_definition_with_control(roles, control)?
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "persisted {catalog} privileges reference missing role incarnation {}",
                identity.oid
            ))
        })
}
