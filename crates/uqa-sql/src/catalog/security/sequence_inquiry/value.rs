//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict arguments, retained role subjects and target diagnostics for sequence privilege inquiry.

use super::{
    acl, role_has_privilege, PrivilegeCheck, RelationIdentity, RelationResolution,
    RoleCatalogGuards, RoleReferenceNames, RoleSubject, SQLError, SequencePrivilegeResolution,
    SequenceSecurityCatalog, Value,
};
use crate::catalog::roles::RoleReference;
use uqa_core::catalog_acl::AclGrantee;

pub struct SequencePrivilegeArguments<'a> {
    subject: Option<&'a Value>,
    sequence: &'a Value,
    privilege: &'a Value,
}

#[derive(Clone, Copy)]
pub enum SequencePrivilegeTarget<'a> {
    Name(&'a str),
    Oid(i64),
}

pub struct SequencePrivilegeRequest<'a> {
    pub target: SequencePrivilegeTarget<'a>,
    subject: Option<RoleReference>,
    checks: Vec<PrivilegeCheck>,
}

impl<'a> SequencePrivilegeArguments<'a> {
    pub fn parse(arguments: &'a [Value]) -> Result<Option<Self>, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(None);
        }
        let (subject, sequence, privilege) = match arguments {
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
        Ok(Some(Self {
            subject,
            sequence,
            privilege,
        }))
    }

    pub fn has_explicit_subject(&self) -> bool {
        self.subject.is_some()
    }

    pub fn bind(
        self,
        names: &dyn RoleReferenceNames,
        roles: &dyn RoleCatalogGuards,
    ) -> Result<SequencePrivilegeRequest<'a>, SQLError> {
        let subject = match self.subject {
            Some(value) => super::super::role_bindings::bind_inquiry_subject(
                value,
                &roles.role_definitions(),
                "has_sequence_privilege",
            )?,
            None => Some(names.current_role()),
        };
        let privilege = match self.privilege {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_sequence_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = acl::parse_privilege_checks(privilege)?;
        let target = match self.sequence {
            Value::Str(reference) | Value::FixedChar(reference) => {
                SequencePrivilegeTarget::Name(reference)
            }
            Value::Int(oid) => SequencePrivilegeTarget::Oid(*oid),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_sequence_privilege sequence must be text or oid, got {other:?}"
                )))
            }
        };
        Ok(SequencePrivilegeRequest {
            target,
            subject,
            checks,
        })
    }
}

impl SequencePrivilegeRequest<'_> {
    pub fn evaluate(
        &self,
        relation: &RelationIdentity,
        roles: &dyn RoleCatalogGuards,
        catalog: &dyn SequenceSecurityCatalog,
    ) -> Result<Value, SQLError> {
        let security = catalog
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let definitions = roles.role_definitions();
        let memberships = roles.role_memberships();
        let subject: &dyn RoleSubject = self
            .subject
            .as_ref()
            .map_or(&AclGrantee::Public as &dyn RoleSubject, |subject| {
                subject as &dyn RoleSubject
            });
        Ok(Value::Bool(self.checks.iter().any(|check| {
            role_has_privilege(&security, subject, *check, &definitions, &memberships)
        })))
    }
}

impl SequencePrivilegeTarget<'_> {
    pub fn resolve(
        self,
        resolution: &dyn SequencePrivilegeResolution,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        match self {
            Self::Oid(oid) => resolution.sequence_privilege_oid(oid),
            Self::Name(reference) => {
                let name = match resolution.visible_relation_kind(reference)? {
                    RelationResolution::Found(name, "sequence") => name,
                    RelationResolution::Found(_, _) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{reference}\" is not a sequence"),
                        })
                    }
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        })
                    }
                    RelationResolution::MissingRelation => return Err(missing_sequence(reference)),
                };
                let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
                    SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?;
                Ok(Some((name, relation)))
            }
        }
    }
}

pub fn missing_sequence(reference: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P01".into(),
        message: format!("relation \"{reference}\" does not exist"),
    }
}
