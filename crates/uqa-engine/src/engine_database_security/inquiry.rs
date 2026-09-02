//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `has_database_privilege` resolution and evaluation.

use std::collections::BTreeMap;

use super::acl::{parse_privilege_checks, role_has_database_privilege_check};
use super::{Engine, RoleDefinition, SQLError, DATABASE_NAME, DATABASE_OID};
use crate::Value;

impl Engine {
    pub(crate) fn has_database_privilege_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load database privileges for inquiry: {error}"))
        })?;
        let (subject_value, database_value, privilege_value) = match arguments {
            [database, privilege] => (None, database, privilege),
            [subject, database, privilege] => (Some(subject), database, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_database_privilege".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                })
            }
        };
        let current_user = subject_value.is_none().then(|| self.current_user_name());
        let subject = {
            let roles = self.durable.roles.read();
            subject_value.map_or_else(
                || Ok(current_user),
                |value| resolve_database_privilege_role(value, &roles),
            )?
        };
        let database_exists = resolve_database_privilege_target(database_value)?;
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_database_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = parse_privilege_checks(privilege)?;
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let subject_is_superuser = subject.as_ref().is_some_and(|subject| {
            roles
                .get(subject)
                .is_some_and(|role| role.has(uqa_sql::ast::RoleAttribute::Superuser))
        });
        if !database_exists {
            return if subject_is_superuser {
                Ok(Value::Bool(true))
            } else {
                Ok(Value::Null)
            };
        }
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        let security = self.durable.database_security.read();
        Ok(Value::Bool(checks.into_iter().any(|check| {
            role_has_database_privilege_check(&security, &subject, check, &roles, &memberships)
        })))
    }
}

fn resolve_database_privilege_target(value: &Value) -> Result<bool, SQLError> {
    match value {
        Value::Str(name) | Value::FixedChar(name) => {
            if name == DATABASE_NAME {
                Ok(true)
            } else {
                Err(SQLError::Routine {
                    sqlstate: "3D000".into(),
                    message: format!("database \"{name}\" does not exist"),
                })
            }
        }
        Value::Int(oid) => Ok(*oid == DATABASE_OID),
        other => Err(SQLError::TypeMismatch(format!(
            "has_database_privilege database must be text or oid, got {other:?}"
        ))),
    }
}

fn resolve_database_privilege_role(
    value: &Value,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<Option<String>, SQLError> {
    match value {
        Value::Str(name) | Value::FixedChar(name) => {
            if roles.contains_key(name) {
                Ok(Some(name.clone()))
            } else {
                Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{name}\" does not exist"),
                })
            }
        }
        Value::Int(oid) => Ok(roles
            .values()
            .find(|role| role.oid == *oid)
            .map(|role| role.name.clone())),
        other => Err(SQLError::TypeMismatch(format!(
            "has_database_privilege role must be name or oid, got {other:?}"
        ))),
    }
}
