//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible table and column privilege inquiry functions.

use std::collections::BTreeMap;

use uqa_sql::SQLError;

use super::acl::{parse_column_privilege_checks, parse_privilege_checks, role_has_privilege};
use super::columns::role_has_column_privilege as column_privilege_check;
use super::{ResolvedColumnPrivilegeTarget, ResolvedTablePrivilegeTarget, POSTGRES_SYSTEM_COLUMNS};
use crate::engine_capabilities::RelationResolution;
use crate::engine_roles::RoleDefinition;
use crate::{Engine, RelationIdentity, Value};

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

fn resolve_table_privilege_role(
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
            "has_table_privilege role must be name or oid, got {other:?}"
        ))),
    }
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

impl Engine {
    pub(crate) fn has_table_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
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
        let current_user = subject_value.is_none().then(|| self.current_user_name());
        let subject = {
            let roles = self.durable.roles.read();
            subject_value.map_or_else(
                || Ok(current_user),
                |value| resolve_table_privilege_role(value, &roles),
            )?
        };
        let Some(target) = self.resolve_table_privilege_target(table_value)? else {
            return Ok(Value::Null);
        };
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_table_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = parse_privilege_checks(privilege)?;
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        match target {
            ResolvedTablePrivilegeTarget::Table(relation) => {
                let table = self
                    .storage
                    .tables
                    .read()
                    .get(&relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "table `{}` disappeared",
                            relation.qualified_name()
                        ))
                    })?;
                let security = table.security();
                let roles = self.durable.roles.read();
                let memberships = self.durable.role_memberships.read();
                Ok(Value::Bool(checks.into_iter().any(|check| {
                    role_has_privilege(&security, &subject, check, &roles, &memberships)
                })))
            }
            ResolvedTablePrivilegeTarget::View(relation) => {
                let view = self
                    .durable
                    .views
                    .read()
                    .get(&relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "view `{}` disappeared",
                            relation.qualified_name()
                        ))
                    })?;
                let security = view.security();
                let roles = self.durable.roles.read();
                let memberships = self.durable.role_memberships.read();
                Ok(Value::Bool(checks.into_iter().any(|check| {
                    role_has_privilege(&security, &subject, check, &roles, &memberships)
                })))
            }
            ResolvedTablePrivilegeTarget::Sequence(relation) => {
                for check in checks {
                    if self.role_has_sequence_table_privilege(
                        &relation,
                        &subject,
                        check.privilege,
                        check.grant_option,
                    )? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
        }
    }

    pub(crate) fn has_column_privilege_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, table_value, column_value, privilege_value) =
            column_privilege_arguments(arguments)?;
        let current_user = subject_value.is_none().then(|| self.current_user_name());
        let subject = {
            let roles = self.durable.roles.read();
            subject_value.map_or_else(
                || Ok(current_user),
                |value| resolve_table_privilege_role(value, &roles),
            )?
        };
        let Some(target) = self.resolve_table_privilege_target(table_value)? else {
            return Ok(Value::Null);
        };
        let (relation, security, columns, has_system_columns) = match target {
            ResolvedTablePrivilegeTarget::Table(relation) => {
                let table = self
                    .storage
                    .tables
                    .read()
                    .get(&relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "table `{}` disappeared",
                            relation.qualified_name()
                        ))
                    })?;
                let columns: Vec<String> = table
                    .columns
                    .read()
                    .iter()
                    .map(|column| column.name.clone())
                    .collect();
                (relation, table.security(), columns, true)
            }
            ResolvedTablePrivilegeTarget::View(relation) => {
                let view = self
                    .durable
                    .views
                    .read()
                    .get(&relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "view `{}` disappeared",
                            relation.qualified_name()
                        ))
                    })?;
                let columns = view.output_columns.clone().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "loaded view `{}` has no durable public column metadata",
                        relation.qualified_name()
                    ))
                })?;
                (relation, view.security(), columns, false)
            }
            ResolvedTablePrivilegeTarget::Sequence(relation) => {
                return self.has_sequence_column_privilege_value(
                    &relation,
                    subject.as_deref(),
                    column_value,
                    privilege_value,
                );
            }
        };
        let Some(column) =
            resolve_column_privilege_target(&relation, &columns, has_system_columns, column_value)?
        else {
            return Ok(Value::Null);
        };
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_column_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = parse_column_privilege_checks(privilege)?;
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        Ok(Value::Bool(checks.into_iter().any(|check| match &column {
            ResolvedColumnPrivilegeTarget::User(column) => {
                column_privilege_check(&security, column, &subject, check, &roles, &memberships)
            }
            ResolvedColumnPrivilegeTarget::System => {
                role_has_privilege(&security, &subject, check, &roles, &memberships)
            }
        })))
    }

    fn has_sequence_column_privilege_value(
        &self,
        relation: &RelationIdentity,
        subject: Option<&str>,
        column_value: &Value,
        privilege_value: &Value,
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
            return match column_value {
                Value::Str(column) | Value::FixedChar(column) => Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        relation.name
                    ),
                }),
                Value::Int(_) => Ok(Value::Null),
                _ => unreachable!("column value type was validated above"),
            };
        }
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_column_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = parse_column_privilege_checks(privilege)?;
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        for check in checks {
            if self.role_has_sequence_table_privilege(
                relation,
                subject,
                check.privilege,
                check.grant_option,
            )? {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }

    fn resolve_table_privilege_target(
        &self,
        value: &Value,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError> {
        match value {
            Value::Str(reference) | Value::FixedChar(reference) => {
                let (name, kind) = match self.resolve_visible_relation_kind(reference)? {
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
                if !matches!(kind, "table" | "view" | "materialized view" | "sequence") {
                    return Err(SQLError::Unsupported(format!(
                        "has_table_privilege for {kind} is not supported"
                    )));
                }
                let relation = Self::resolved_relation_identity(&name).map_err(|error| {
                    SQLError::Internal(format!("resolve table `{name}`: {error}"))
                })?;
                Ok(Some(match kind {
                    "table" => ResolvedTablePrivilegeTarget::Table(relation),
                    "view" | "materialized view" => ResolvedTablePrivilegeTarget::View(relation),
                    "sequence" => ResolvedTablePrivilegeTarget::Sequence(relation),
                    _ => unreachable!("relation kind was validated above"),
                }))
            }
            Value::Int(oid) => self.resolve_table_privilege_oid(*oid),
            other => Err(SQLError::TypeMismatch(format!(
                "has_table_privilege table must be text or oid, got {other:?}"
            ))),
        }
    }

    fn resolve_table_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError> {
        self.synchronize_table_catalog().map_err(|error| {
            SQLError::Internal(format!("load tables for privilege inquiry: {error}"))
        })?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load views for privilege inquiry: {error}"))
        })?;
        let catalog = self.catalog_read_view();
        let resolution = self.session_execution_view().relation_name_resolution();
        for relation in self.storage.tables.read().keys() {
            if crate::sql::snapshot_table_relation_oid(
                &catalog,
                &resolution,
                &relation.qualified_name(),
            )? == oid
            {
                return Ok(Some(ResolvedTablePrivilegeTarget::Table(relation.clone())));
            }
        }
        for (relation, view) in self.durable.views.read().iter() {
            if crate::sql::view_relation_oid(relation, view.kind) == oid {
                return Ok(Some(ResolvedTablePrivilegeTarget::View(relation.clone())));
            }
        }
        if let Some((_name, relation)) = self.resolve_sequence_privilege_oid(oid)? {
            return Ok(Some(ResolvedTablePrivilegeTarget::Sequence(relation)));
        }
        if let Some((name, kind)) = crate::sql::resolve_regclass_kind_by_oid(self, oid)? {
            return Err(SQLError::Unsupported(format!(
                "has_table_privilege for {kind} `{name}` is not supported"
            )));
        }
        Ok(None)
    }
}
