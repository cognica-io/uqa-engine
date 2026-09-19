//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function privilege inquiry semantics with strict name resolution and nullable OID lookup.

use super::security::routine_privilege_allowed;
use crate::catalog::roles::{identity::RoleSubject, RoleReference};
use crate::{
    ast::{RoleAttribute, RoutineAclEntry},
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::Value;

pub struct RoutinePrivileges<'a> {
    pub owner: crate::catalog::roles::RoleIdentity,
    pub execute_acl: Option<&'a [RoutineAclEntry]>,
}

pub trait RoutinePrivilegeCatalog {
    fn resolve_routine_name(&self, name: &str) -> Result<i64, SQLError>;
    fn routine_privileges(&self, oid: i64) -> Result<Option<RoutinePrivileges<'_>>, SQLError>;
}

pub struct RoutinePrivilegeInquiry<'a> {
    pub current_user: &'a RoleReference,
    pub roles: &'a BTreeMap<String, RoleDefinition>,
    pub memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
    pub catalog: &'a dyn RoutinePrivilegeCatalog,
}

impl RoutinePrivilegeInquiry<'_> {
    pub fn has_function_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.contains(&Value::Null) {
            return Ok(Value::Null);
        }
        let (subject, target, privilege) = match arguments {
            [target, privilege] => (Some(self.current_user.clone()), target, privilege),
            [subject, target, privilege] => (self.resolve_role(subject)?, target, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_function_privilege".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                })
            }
        };
        let (oid, missing_is_null) = match target {
            Value::Str(name) | Value::FixedChar(name) => {
                let oid = self.catalog.resolve_routine_name(name)?;
                if oid == 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "42883".into(),
                        message: format!("function \"{name}\" does not exist"),
                    });
                }
                (oid, false)
            }
            Value::Int(oid) => (*oid, true),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_function_privilege function must be text or oid, got {other:?}"
                )))
            }
        };
        let checks = match privilege {
            Value::Str(value) | Value::FixedChar(value) => parse_privileges(value)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_function_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        if subject
            .as_ref()
            .and_then(|subject| subject.role_definition(self.roles))
            .is_some_and(|role| role.attributes.contains(&RoleAttribute::Superuser))
        {
            return Ok(Value::Bool(true));
        }
        let Some(security) = self.catalog.routine_privileges(oid)? else {
            return if missing_is_null {
                Ok(Value::Null)
            } else {
                Err(SQLError::Internal(format!(
                    "cache lookup failed for function {oid}"
                )))
            };
        };
        Ok(Value::Bool(checks.into_iter().any(|grant_option| {
            routine_privilege_allowed(
                &security.owner,
                security.execute_acl,
                grant_option,
                false,
                |role| {
                    subject.as_ref().is_some_and(|subject| {
                        role_inherits(self.roles, self.memberships, subject, role)
                    })
                },
            )
        })))
    }

    fn resolve_role(&self, value: &Value) -> Result<Option<RoleReference>, SQLError> {
        match value {
            Value::Str(name) | Value::FixedChar(name) if name == "public" => Ok(None),
            Value::Str(name) | Value::FixedChar(name) => {
                if self.roles.contains_key(name) {
                    Ok(Some(name.clone().into()))
                } else {
                    Err(SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: format!("role \"{name}\" does not exist"),
                    })
                }
            }
            Value::Int(oid) => Ok(self
                .roles
                .values()
                .find(|role| role.oid == *oid)
                .map(|role| role.name.clone().into())),
            other => Err(SQLError::TypeMismatch(format!(
                "has_function_privilege role must be name or oid, got {other:?}"
            ))),
        }
    }
}

fn parse_privileges(value: &str) -> Result<Vec<bool>, SQLError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if item.eq_ignore_ascii_case("EXECUTE") {
                Ok(false)
            } else if item.eq_ignore_ascii_case("EXECUTE WITH GRANT OPTION") {
                Ok(true)
            } else {
                Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("unrecognized privilege type: \"{item}\""),
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
