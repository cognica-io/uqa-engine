//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence privilege inquiry, target binding and access checks.

use super::{
    sequence::{self as acl, role_has_privilege, AclPrivilege, PrivilegeCheck},
    SequenceSecurity,
};
use crate::{
    catalog::{
        resolution::RelationResolution,
        roles::{guards::RoleCatalogGuards, RoleDefinition, RoleReferenceNames},
    },
    SQLError,
};
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::{RelationIdentity, Value};

pub type SequenceSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, SequenceSecurity>> + 'a>;
pub trait SequenceSecurityCatalog {
    fn security_read(&self) -> SequenceSecurityRead<'_>;
}
pub trait SequencePrivilegeResolution {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError>;
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError>;
}
pub struct SequencePrivilegeInquiry<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub security: &'a dyn SequenceSecurityCatalog,
    pub resolution: &'a dyn SequencePrivilegeResolution,
}

impl SequencePrivilegeInquiry<'_> {
    pub fn ensure_sequence_nextval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Update],
        )
    }

    pub fn ensure_sequence_currval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Select],
        )
    }

    pub fn ensure_sequence_setval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(name, relation, &[AclPrivilege::Update])
    }

    fn ensure_sequence_value_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
        privileges: &[AclPrivilege],
    ) -> Result<(), SQLError> {
        let security = self
            .security
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        let current_user = self.names.current_user_name();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        if privileges.iter().any(|privilege| {
            role_has_privilege(
                &security,
                &current_user,
                PrivilegeCheck {
                    privilege: *privilege,
                    grant_option: false,
                },
                &roles,
                &memberships,
            )
        }) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for sequence {}", relation.name),
        })
    }

    pub fn has_sequence_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, sequence_value, privilege_value) = match arguments {
            [sequence, privilege] => (None, sequence, privilege),
            [subject, sequence, privilege] => (Some(subject), sequence, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_sequence_privilege".into(),
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
                |value| resolve_sequence_privilege_role(value, &roles),
            )?
        };
        let Some((_name, relation)) = self.resolve_sequence_privilege_target(sequence_value)?
        else {
            return Ok(Value::Null);
        };
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_sequence_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = acl::parse_privilege_checks(privilege)?;
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        let security = self
            .security
            .security_read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        Ok(Value::Bool(checks.into_iter().any(|check| {
            role_has_privilege(&security, &subject, check, &roles, &memberships)
        })))
    }

    pub fn role_has_sequence_table_privilege(
        &self,
        relation: &RelationIdentity,
        subject: &str,
        privilege: super::table::TableAclPrivilege,
        grant_option: bool,
    ) -> Result<bool, SQLError> {
        let privilege = match privilege {
            super::table::TableAclPrivilege::Select => AclPrivilege::Select,
            super::table::TableAclPrivilege::Update => AclPrivilege::Update,
            super::table::TableAclPrivilege::Insert
            | super::table::TableAclPrivilege::Delete
            | super::table::TableAclPrivilege::Truncate
            | super::table::TableAclPrivilege::References
            | super::table::TableAclPrivilege::Trigger
            | super::table::TableAclPrivilege::Maintain => return Ok(false),
        };
        let security = self
            .security
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        Ok(role_has_privilege(
            &security,
            subject,
            PrivilegeCheck {
                privilege,
                grant_option,
            },
            &roles,
            &memberships,
        ))
    }

    fn resolve_sequence_privilege_target(
        &self,
        value: &Value,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        match value {
            Value::Str(reference) | Value::FixedChar(reference) => {
                let (name, kind) = match self.resolution.visible_relation_kind(reference)? {
                    RelationResolution::Found(name, kind) => (name, kind),
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        });
                    }
                    RelationResolution::MissingRelation => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("relation \"{reference}\" does not exist"),
                        });
                    }
                };
                if kind != "sequence" {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: format!("\"{reference}\" is not a sequence"),
                    });
                }
                let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
                    SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?;
                Ok(Some((name, relation)))
            }
            Value::Int(oid) => self.resolution.sequence_privilege_oid(*oid),
            other => Err(SQLError::TypeMismatch(format!(
                "has_sequence_privilege sequence must be text or oid, got {other:?}"
            ))),
        }
    }

    pub fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError> {
        let owner = self
            .security
            .security_read()
            .get(relation)
            .map(|security| security.role_owner.clone())
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        crate::schema::sequences::ownership::require_sequence_ownership(&relation.name, {
            let current = self.names.current_user_name();
            let roles = self.roles.role_definitions();
            let memberships = self.roles.role_memberships();
            crate::catalog::roles::role_inherits(&roles, &memberships, &current, &owner)
        })?;
        Ok(owner)
    }
}

fn resolve_sequence_privilege_role(
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
            "has_sequence_privilege role must be name or oid, got {other:?}"
        ))),
    }
}
