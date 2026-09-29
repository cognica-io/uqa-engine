//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `has_type_privilege`: a type named by text must exist, an unknown OID yields NULL, and an array type answers with its element type's privileges.

use super::object_acl;
use crate::ast::{ObjectAclEntry, RoleAttribute};
use crate::catalog::roles::{
    identity::RoleSubject, role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey,
    RoleReference,
};
use crate::SQLError;
use std::collections::BTreeMap;
use uqa_core::{catalog_role::RoleIdentity, Value};

/// The owner and explicit `USAGE` ACL governing a type. `None` is the default ACL.
pub struct TypePrivileges<'a> {
    pub owner: RoleIdentity,
    pub usage_acl: Option<&'a [ObjectAclEntry]>,
}

pub trait TypePrivilegeCatalog {
    /// The OID of a type name as `regtype` input resolves it, or 42704.
    fn resolve_type_name(&self, name: &str) -> Result<u32, SQLError>;
    /// The privileges of the type, or of the element type of a generated array. `None` when no type has the OID.
    fn type_privileges(&self, oid: u32) -> Option<TypePrivileges<'_>>;
}

pub struct TypePrivilegeInquiry<'a> {
    pub current_user: &'a RoleReference,
    pub roles: &'a BTreeMap<String, RoleDefinition>,
    pub memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
    pub catalog: &'a dyn TypePrivilegeCatalog,
}

impl TypePrivilegeInquiry<'_> {
    pub fn has_type_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.contains(&Value::Null) {
            return Ok(Value::Null);
        }
        let (subject, target, privilege) = match arguments {
            [target, privilege] => (Some(self.current_user.clone()), target, privilege),
            [subject, target, privilege] => (self.resolve_role(subject)?, target, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_type_privilege".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                })
            }
        };
        let oid = match target {
            Value::Str(name) | Value::FixedChar(name) => self.catalog.resolve_type_name(name)?,
            Value::Int(oid) => match u32::try_from(*oid) {
                Ok(oid) => oid,
                Err(_) => return Ok(Value::Null),
            },
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_type_privilege type must be text or oid, got {other:?}"
                )))
            }
        };
        let checks = match privilege {
            Value::Str(value) | Value::FixedChar(value) => parse_privileges(value)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_type_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let Some(privileges) = self.catalog.type_privileges(oid) else {
            return Ok(Value::Null);
        };
        let superuser = subject
            .as_ref()
            .and_then(|subject| subject.role_definition(self.roles))
            .is_some_and(|role| role.attributes.contains(&RoleAttribute::Superuser));
        Ok(Value::Bool(checks.into_iter().any(|grant_option| {
            object_acl::privilege_allowed(
                &privileges.owner,
                privileges.usage_acl,
                grant_option,
                superuser,
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
                "has_type_privilege role must be name or oid, got {other:?}"
            ))),
        }
    }
}

/// `convert_type_priv_string`: a comma-separated list of `USAGE` and `USAGE WITH GRANT OPTION`.
fn parse_privileges(value: &str) -> Result<Vec<bool>, SQLError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if item.eq_ignore_ascii_case("USAGE") {
                Ok(false)
            } else if item.eq_ignore_ascii_case("USAGE WITH GRANT OPTION") {
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
