//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table and column privilege inquiry and `PostgreSQL` relation/attribute binding rules.

use super::{
    columns::role_has_column_privilege as column_privilege_check,
    sequence_inquiry::SequenceTablePrivilegeInquiry,
    table::{
        parse_column_privilege_checks, parse_privilege_checks, role_has_privilege,
        TablePrivilegeCheck,
    },
    TableSecurity,
};
use crate::catalog::roles::{identity::RoleSubject, RoleReference};
use crate::{
    catalog::{
        resolution::RelationResolution,
        roles::{guards::RoleCatalogGuards, RoleDefinition, RoleReferenceNames},
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{catalog_acl::AclGrantee, RelationIdentity, Value};

pub trait TablePrivilegeCatalog {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError>;
    fn resolve_table_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError>;
    fn table_privilege_security(
        &self,
        target: &ResolvedTablePrivilegeTarget,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<TableSecurity, SQLError>;
    fn column_privilege_relation(
        &self,
        target: &ResolvedTablePrivilegeTarget,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<ColumnPrivilegeRelation, SQLError>;
}

pub struct TablePrivilegeInquiry<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub sequences: &'a dyn SequenceTablePrivilegeInquiry,
    pub catalog: &'a dyn TablePrivilegeCatalog,
}

pub enum ResolvedTablePrivilegeTarget {
    System(crate::catalog::SystemRelation),
    Table(RelationIdentity),
    View(RelationIdentity),
    ForeignTable(RelationIdentity),
    Sequence(RelationIdentity),
}

enum ResolvedColumnPrivilegeTarget {
    User(String),
    System,
}

const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["ctid", "xmin", "cmin", "xmax", "cmax", "tableoid"];

pub struct ColumnPrivilegeRelation {
    pub relation: RelationIdentity,
    pub security: TableSecurity,
    pub columns: Vec<String>,
    pub has_system_columns: bool,
}

fn resolve_column_privilege_target(
    relation: &RelationIdentity,
    columns: &[String],
    has_system_columns: bool,
    value: &Value,
) -> Result<Option<ResolvedColumnPrivilegeTarget>, SQLError> {
    match value {
        Value::Str(column) | Value::FixedChar(column) => {
            if columns.iter().any(|definition| definition == column) {
                Ok(Some(ResolvedColumnPrivilegeTarget::User(column.clone())))
            } else if has_system_columns && POSTGRES_SYSTEM_COLUMNS.contains(&column.as_str()) {
                Ok(Some(ResolvedColumnPrivilegeTarget::System))
            } else {
                Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        relation.name
                    ),
                })
            }
        }
        Value::Int(attnum) if *attnum > 0 => Ok(usize::try_from(*attnum - 1)
            .ok()
            .and_then(|index| columns.get(index))
            .map(|column| ResolvedColumnPrivilegeTarget::User(column.clone()))),
        Value::Int(attnum) if has_system_columns && (-6..=-1).contains(attnum) => {
            Ok(Some(ResolvedColumnPrivilegeTarget::System))
        }
        Value::Int(_) => Ok(None),
        other => Err(SQLError::TypeMismatch(format!(
            "has_column_privilege column must be text or smallint, got {other:?}"
        ))),
    }
}

fn privilege_text<'a>(function: &str, value: &'a Value) -> Result<&'a str, SQLError> {
    match value {
        Value::Str(privilege) | Value::FixedChar(privilege) => Ok(privilege),
        other => Err(SQLError::TypeMismatch(format!(
            "{function} privilege must be text, got {other:?}"
        ))),
    }
}

fn table_privilege_checks(value: &Value) -> Result<Vec<TablePrivilegeCheck>, SQLError> {
    parse_privilege_checks(privilege_text("has_table_privilege", value)?)
}

fn column_privilege_checks(value: &Value) -> Result<Vec<TablePrivilegeCheck>, SQLError> {
    parse_column_privilege_checks(privilege_text("has_column_privilege", value)?)
}

fn column_privilege_arguments(
    arguments: &[Value],
) -> Result<(Option<&Value>, &Value, &Value, &Value), SQLError> {
    match arguments {
        [table, column, privilege] => Ok((None, table, column, privilege)),
        [subject, table, column, privilege] => Ok((Some(subject), table, column, privilege)),
        _ => Err(SQLError::BadArity {
            name: "has_column_privilege".into(),
            expected: "3 or 4".into(),
            actual: arguments.len(),
        }),
    }
}

impl TablePrivilegeInquiry<'_> {
    pub fn has_table_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, table_value, privilege_value) = match arguments {
            [table, privilege] => (None, table, privilege),
            [subject, table, privilege] => (Some(subject), table, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_table_privilege".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                })
            }
        };
        let subject = self.bind_subject(subject_value, "has_table_privilege")?;
        let subject: &dyn RoleSubject = subject
            .as_ref()
            .map_or(&AclGrantee::Public as &dyn RoleSubject, |subject| {
                subject as &dyn RoleSubject
            });
        let checks = matches!(table_value, Value::Int(_))
            .then(|| table_privilege_checks(privilege_value))
            .transpose()?;
        let Some(target) = self.resolve_table_privilege_target(table_value)? else {
            return Ok(Value::Null);
        };
        let checks = checks.map_or_else(|| table_privilege_checks(privilege_value), Ok)?;
        if let ResolvedTablePrivilegeTarget::Sequence(relation) = &target {
            return self
                .sequences
                .sequence_table_privileges(relation, subject, &checks)
                .map(Value::Bool);
        }
        let roles = self.roles.role_definitions();
        let security = self.catalog.table_privilege_security(&target, &roles)?;
        let memberships = self.roles.role_memberships();
        Ok(Value::Bool(checks.into_iter().any(|check| {
            if let ResolvedTablePrivilegeTarget::System(relation) = target {
                return super::system_relations::has_table_privilege(
                    relation,
                    &security,
                    subject,
                    check,
                    &roles,
                    &memberships,
                );
            }
            role_has_privilege(&security, subject, check, &roles, &memberships)
        })))
    }

    pub fn has_column_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, table_value, column_value, privilege_value) =
            column_privilege_arguments(arguments)?;
        let subject = self.bind_subject(subject_value, "has_column_privilege")?;
        let subject: &dyn RoleSubject = subject
            .as_ref()
            .map_or(&AclGrantee::Public as &dyn RoleSubject, |subject| {
                subject as &dyn RoleSubject
            });
        let mut checks = (matches!(table_value, Value::Int(_))
            && matches!(column_value, Value::Int(_)))
        .then(|| column_privilege_checks(privilege_value))
        .transpose()?;
        let Some(target) = self.resolve_table_privilege_target(table_value)? else {
            if checks.is_none() {
                column_privilege_checks(privilege_value)?;
            }
            return Ok(Value::Null);
        };
        if checks.is_none() && matches!(column_value, Value::Int(_)) {
            checks = Some(column_privilege_checks(privilege_value)?);
        }
        if let ResolvedTablePrivilegeTarget::Sequence(relation) = &target {
            return self.has_sequence_column_privilege_value(
                relation,
                subject,
                column_value,
                privilege_value,
                checks,
            );
        }
        let roles = self.roles.role_definitions();
        let metadata = self.catalog.column_privilege_relation(&target, &roles)?;
        let column = resolve_column_privilege_target(
            &metadata.relation,
            &metadata.columns,
            metadata.has_system_columns,
            column_value,
        )?;
        let checks = checks.map_or_else(|| column_privilege_checks(privilege_value), Ok)?;
        let Some(column) = column else {
            return Ok(Value::Null);
        };
        let memberships = self.roles.role_memberships();
        Ok(Value::Bool(checks.into_iter().any(|check| {
            if let ResolvedTablePrivilegeTarget::System(relation) = target {
                return match &column {
                    ResolvedColumnPrivilegeTarget::User(column) => {
                        super::system_relations::has_column_privilege(
                            relation,
                            &metadata.security,
                            column,
                            subject,
                            check,
                            &roles,
                            &memberships,
                        )
                    }
                    ResolvedColumnPrivilegeTarget::System => {
                        super::system_relations::has_table_privilege(
                            relation,
                            &metadata.security,
                            subject,
                            check,
                            &roles,
                            &memberships,
                        )
                    }
                };
            }
            match &column {
                ResolvedColumnPrivilegeTarget::User(column) => column_privilege_check(
                    &metadata.security,
                    column,
                    subject,
                    check,
                    &roles,
                    &memberships,
                ),
                ResolvedColumnPrivilegeTarget::System => {
                    role_has_privilege(&metadata.security, subject, check, &roles, &memberships)
                }
            }
        })))
    }

    fn has_sequence_column_privilege_value(
        &self,
        relation: &RelationIdentity,
        subject: &dyn RoleSubject,
        column_value: &Value,
        privilege_value: &Value,
        checks: Option<Vec<TablePrivilegeCheck>>,
    ) -> Result<Value, SQLError> {
        let valid_column = match column_value {
            Value::Str(column) | Value::FixedChar(column) => {
                matches!(column.as_str(), "last_value" | "log_cnt" | "is_called")
                    || POSTGRES_SYSTEM_COLUMNS.contains(&column.as_str())
            }
            Value::Int(attnum) => (1..=3).contains(attnum) || (-6..=-1).contains(attnum),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_column_privilege column must be text or smallint, got {other:?}"
                )))
            }
        };
        if !valid_column {
            if let Value::Str(column) | Value::FixedChar(column) = column_value {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        relation.name
                    ),
                });
            }
        }
        let checks = checks.map_or_else(|| column_privilege_checks(privilege_value), Ok)?;
        if !valid_column {
            return Ok(Value::Null);
        }
        self.sequences
            .sequence_table_privileges(relation, subject, &checks)
            .map(Value::Bool)
    }

    fn bind_subject(
        &self,
        value: Option<&Value>,
        function: &str,
    ) -> Result<Option<RoleReference>, SQLError> {
        match value {
            None => Ok(Some(self.names.current_role())),
            Some(value) => {
                let roles = self.roles.inquiry_role_definitions()?;
                super::role_bindings::bind_inquiry_subject(value, &roles, function)
            }
        }
    }

    fn resolve_table_privilege_target(
        &self,
        value: &Value,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError> {
        match value {
            Value::Str(reference) | Value::FixedChar(reference) => {
                let (name, kind) = match self.catalog.visible_relation_kind(reference)? {
                    RelationResolution::Found(name, kind) => (name, kind),
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        })
                    }
                    RelationResolution::MissingRelation => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("relation \"{reference}\" does not exist"),
                        })
                    }
                };
                if let Some(relation) = crate::catalog::SystemRelation::from_qualified_name(&name) {
                    return Ok(Some(ResolvedTablePrivilegeTarget::System(relation)));
                }
                if !matches!(
                    kind,
                    "table" | "view" | "materialized view" | "foreign table" | "sequence"
                ) {
                    return Err(SQLError::Unsupported(format!(
                        "has_table_privilege for {kind} is not supported"
                    )));
                }
                let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
                    SQLError::Internal(format!("resolve table `{name}`: {error}"))
                })?;
                Ok(Some(match kind {
                    "table" => ResolvedTablePrivilegeTarget::Table(relation),
                    "view" | "materialized view" => ResolvedTablePrivilegeTarget::View(relation),
                    "foreign table" => ResolvedTablePrivilegeTarget::ForeignTable(relation),
                    "sequence" => ResolvedTablePrivilegeTarget::Sequence(relation),
                    _ => unreachable!("relation kind was validated above"),
                }))
            }
            Value::Int(oid) => self.catalog.resolve_table_privilege_oid(*oid),
            other => Err(SQLError::TypeMismatch(format!(
                "has_table_privilege table must be text or oid, got {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
