//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `has_schema_privilege` resolution and evaluation.

use std::collections::{BTreeMap, BTreeSet};

use super::acl::{parse_privilege_checks, role_has_schema_privilege_check};
use super::{Engine, RoleDefinition, SQLError, SchemaSecurity};
use crate::Value;

impl Engine {
    pub(crate) fn has_schema_privilege_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, schema_value, privilege_value) = match arguments {
            [schema, privilege] => (None, schema, privilege),
            [subject, schema, privilege] => (Some(subject), schema, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_schema_privilege".into(),
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
                |value| resolve_schema_privilege_role(value, &roles),
            )?
        };
        let schema = self.resolve_schema_privilege_target(schema_value)?;
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_schema_privilege privilege must be text, got {other:?}"
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
        let Some(schema) = schema else {
            return if subject_is_superuser {
                Ok(Value::Bool(true))
            } else {
                Ok(Value::Null)
            };
        };
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        let security = self.schema_security_for_inquiry(&schema).ok_or_else(|| {
            SQLError::Internal(format!("schema `{schema}` has no security metadata"))
        })?;
        Ok(Value::Bool(checks.into_iter().any(|check| {
            role_has_schema_privilege_check(&security, &subject, check, &roles, &memberships)
        })))
    }

    fn resolve_schema_privilege_target(&self, value: &Value) -> Result<Option<String>, SQLError> {
        let names = self.schema_privilege_namespace_names()?;
        match value {
            Value::Str(name) | Value::FixedChar(name) => {
                if names.contains(name) {
                    Ok(Some(name.clone()))
                } else {
                    Err(SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{name}\" does not exist"),
                    })
                }
            }
            Value::Int(oid) => Ok(names
                .into_iter()
                .find(|name| crate::sql::schema_object_oid(name) == *oid)),
            other => Err(SQLError::TypeMismatch(format!(
                "has_schema_privilege schema must be text or oid, got {other:?}"
            ))),
        }
    }

    fn schema_privilege_namespace_names(&self) -> Result<BTreeSet<String>, SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load schemas for privilege inquiry: {error}"))
        })?;
        let mut names = BTreeSet::from([
            "pg_catalog".to_string(),
            "information_schema".to_string(),
            "ag_catalog".to_string(),
        ]);
        names.extend(self.durable.schemas.read().keys().cloned());
        names.extend(self.durable.graphs.read().keys().cloned());
        if self.temporary_namespace_allocated() {
            names.insert(self.temporary_schema_name());
        }
        Ok(names)
    }

    fn schema_security_for_inquiry(&self, schema: &str) -> Option<SchemaSecurity> {
        self.schema_security_for_privilege(schema)
    }
}

fn resolve_schema_privilege_role(
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
            "has_schema_privilege role must be name or oid, got {other:?}"
        ))),
    }
}
