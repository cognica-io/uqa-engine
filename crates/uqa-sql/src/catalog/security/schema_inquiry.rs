//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema privilege inquiry, namespace visibility and default security rules.

use super::{
    schema::{
        parse_privilege_checks, role_has_schema_privilege, role_has_schema_privilege_check,
        schema_security_with_public_privileges, SchemaAclPrivilege,
    },
    SchemaSecurity,
};
use crate::{
    catalog::roles::{guards::RoleCatalogGuards, RoleDefinition, RoleReferenceNames},
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::Value;

pub type SchemaRegistryRead<'a> =
    Box<dyn std::ops::Deref<Target = BTreeMap<String, SchemaSecurity>> + 'a>;

/// Metadata-only graph names held under the caller's original registry read guard.
pub trait GraphNamespaceRead {
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_>;
    fn contains(&self, name: &str) -> bool;
}

pub trait SchemaPrivilegeCatalog {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError>;
    fn schemas(&self) -> SchemaRegistryRead<'_>;
    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_>;
    fn temporary_namespace_allocated(&self) -> bool;
    fn temporary_schema_name(&self) -> String;
}

pub struct SchemaPrivilegeInquiry<'a> {
    pub catalog: &'a dyn SchemaPrivilegeCatalog,
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
}

impl SchemaPrivilegeInquiry<'_> {
    pub fn schema_has_privilege_for_role(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> bool {
        let Some(security) = self.schema_security_for_privilege(schema) else {
            return false;
        };
        role_has_schema_privilege(
            &security,
            role,
            privilege,
            &self.roles.role_definitions(),
            &self.roles.role_memberships(),
        )
    }

    pub fn schema_security_for_privilege(&self, schema: &str) -> Option<SchemaSecurity> {
        if let Some(security) = self.catalog.schemas().get(schema) {
            return Some(security.clone());
        }
        match schema {
            "pg_catalog" | "information_schema" => {
                Some(schema_security_with_public_privileges(false))
            }
            "ag_catalog" => Some(SchemaSecurity::legacy("ag_catalog")),
            name if name == self.catalog.temporary_schema_name() => {
                Some(schema_security_with_public_privileges(true))
            }
            name if self.catalog.graphs().contains(name) => Some(SchemaSecurity::legacy(name)),
            _ => None,
        }
    }

    pub fn require_schema_privilege(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        if self.schema_has_privilege_for_role(schema, role, privilege) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for schema {schema}"),
        })
    }

    pub fn has_schema_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
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
        let current_user = subject_value
            .is_none()
            .then(|| self.names.current_user_name());
        let subject = {
            let roles = self.roles.role_definitions();
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
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        let subject_is_superuser = subject.as_ref().is_some_and(|subject| {
            roles
                .get(subject)
                .is_some_and(|role| role.has(crate::ast::RoleAttribute::Superuser))
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
        let security = self.schema_security_for_privilege(&schema).ok_or_else(|| {
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
                .find(|name| crate::catalog::oids::schema_oid(name) == *oid)),
            other => Err(SQLError::TypeMismatch(format!(
                "has_schema_privilege schema must be text or oid, got {other:?}"
            ))),
        }
    }

    fn schema_privilege_namespace_names(&self) -> Result<BTreeSet<String>, SQLError> {
        self.catalog.refresh_namespace_catalog()?;
        let mut names = BTreeSet::from([
            "pg_catalog".to_string(),
            "information_schema".to_string(),
            "ag_catalog".to_string(),
        ]);
        names.extend(self.catalog.schemas().keys().cloned());
        names.extend(self.catalog.graphs().names().map(str::to_owned));
        if self.catalog.temporary_namespace_allocated() {
            names.insert(self.catalog.temporary_schema_name());
        }
        Ok(names)
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
