//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared relation and column ACL mutation machinery for `GRANT` and `REVOKE`.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_sql::ast::{GrantTableStmt, SequencePrivilege, TablePrivilege, TableRevokeBehavior};
use uqa_sql::SQLError;

use super::acl::{grant_acl, revoke_acl, select_acl_grantor, RequestedTablePrivileges};
use super::columns::{grant_column_acl, revoke_column_acl, select_column_acl_grantor};
use super::{validate_table_security_invariants, TableAclPrivilege};
use crate::engine_roles::{RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::TableSecurity;
use crate::{Engine, RelationIdentity, StoredView, TableState};

pub(super) struct ResolvedTableGrantTarget {
    pub(super) requested: String,
    pub(super) name: String,
    pub(super) relation: RelationIdentity,
    pub(super) kind: &'static str,
}

fn apply_table_acl(
    statement: &GrantTableStmt,
    grantees: &[String],
    privileges: &[TableAclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &TableSecurity,
) -> Result<(TableSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|privilege| {
            (
                *privilege,
                select_acl_grantor(current, *privilege, current_user, roles, memberships),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == TableRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

fn apply_column_acl(
    statement: &GrantTableStmt,
    grantees: &[String],
    privileges: &[(TableAclPrivilege, String)],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &TableSecurity,
) -> Result<(TableSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|(privilege, column)| {
            (
                *privilege,
                column.clone(),
                select_column_acl_grantor(
                    current,
                    column,
                    *privilege,
                    current_user,
                    roles,
                    memberships,
                ),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, _, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, column, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_column_acl(
                &mut next,
                &column,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_column_acl(
                &mut next,
                &column,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == TableRevokeBehavior::Cascade,
            )?;
        }
    }
    next.column_acls.retain(|_, acl| !acl.is_empty());
    Ok((next, grantable))
}

pub(super) struct TableGrantApplication<'a> {
    pub(super) statement: &'a GrantTableStmt,
    pub(super) grantees: &'a [String],
    pub(super) requested: &'a RequestedTablePrivileges,
    pub(super) current_user: &'a str,
    pub(super) roles: &'a BTreeMap<String, RoleDefinition>,
    pub(super) memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
}

impl TableGrantApplication<'_> {
    fn apply(&self, current: &TableSecurity) -> Result<(TableSecurity, usize), SQLError> {
        let (next, table_grantable) = apply_table_acl(
            self.statement,
            self.grantees,
            &self.requested.table,
            self.current_user,
            self.roles,
            self.memberships,
            current,
        )?;
        let (next, column_grantable) = apply_column_acl(
            self.statement,
            self.grantees,
            &self.requested.columns,
            self.current_user,
            self.roles,
            self.memberships,
            &next,
        )?;
        Ok((next, table_grantable + column_grantable))
    }

    fn record_warning(
        &self,
        grantable: usize,
        relation: &RelationIdentity,
        notices: &mut Vec<(&'static str, String)>,
    ) {
        let requested = self.requested.table.len() + self.requested.columns.len();
        if grantable != requested {
            notices.push(table_acl_warning(
                self.statement.is_grant,
                grantable != 0,
                &relation.name,
            ));
        }
    }
}

pub(super) type TablePrivilegeUpdate = (String, Arc<TableState>, TableSecurity);
pub(super) type ViewPrivilegeUpdate = (RelationIdentity, StoredView);
pub(super) type ForeignTablePrivilegeUpdate = (RelationIdentity, TableSecurity);
pub(super) type ForeignTableGrantTarget<'a> =
    (&'a ResolvedTableGrantTarget, TableSecurity, Vec<String>);

pub(super) fn validated_table_grant_targets<'a>(
    engine: &Engine,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<(&'a ResolvedTableGrantTarget, Arc<TableState>)>, SQLError> {
    let tables = engine.storage.tables.read();
    targets
        .iter()
        .filter(|target| target.kind == "table")
        .map(|target| {
            let table = tables.get(&target.relation).cloned().ok_or_else(|| {
                SQLError::Internal(format!("table `{}` disappeared", target.name))
            })?;
            let columns = table
                .columns
                .read()
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            validate_requested_columns(&target.relation, &columns, requested)?;
            Ok((target, table))
        })
        .collect()
}

pub(super) fn validated_view_grant_targets<'a>(
    engine: &Engine,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<(&'a ResolvedTableGrantTarget, StoredView)>, SQLError> {
    let views = engine.durable.views.read();
    let selected = targets
        .iter()
        .filter(|target| matches!(target.kind, "view" | "materialized view"))
        .map(|target| {
            let view = views
                .get(&target.relation)
                .cloned()
                .ok_or_else(|| SQLError::Internal(format!("view `{}` disappeared", target.name)))?;
            Ok((target, view))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    drop(views);
    for (target, view) in &selected {
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{}` has no durable public column metadata",
                target.relation.qualified_name()
            ))
        })?;
        validate_requested_columns(&target.relation, columns, requested)?;
    }
    Ok(selected)
}

pub(super) fn persist_table_privilege_updates(
    engine: &Engine,
    updates: &[TablePrivilegeUpdate],
    view_updates: &[ViewPrivilegeUpdate],
    foreign_updates: &[ForeignTablePrivilegeUpdate],
) -> Result<(), SQLError> {
    for (name, table, security) in updates {
        engine.persist_table_security(name, table, security)?;
    }
    if let Some(catalog) = engine.storage.catalog.as_ref() {
        for (relation, view) in view_updates {
            if view.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let row = crate::engine_session::catalog_view_row(relation, view).map_err(|error| {
                SQLError::Internal(format!(
                    "serialize view privileges for `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
            catalog.save_view(&row).map_err(|error| {
                SQLError::Internal(format!(
                    "persist view privileges for `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
        }
    }
    for (relation, security) in foreign_updates {
        engine.persist_foreign_table_security(relation, security)?;
    }
    Ok(())
}

pub(super) fn validated_foreign_table_grant_targets<'a>(
    engine: &Engine,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<ForeignTableGrantTarget<'a>>, SQLError> {
    let tables = engine.durable.foreign_tables.read();
    let securities = engine.durable.foreign_table_security.read();
    targets
        .iter()
        .filter(|target| target.kind == "foreign table")
        .map(|target| {
            let table = tables.get(&target.relation).ok_or_else(|| {
                SQLError::Internal(format!("foreign table `{}` disappeared", target.name))
            })?;
            let security = securities.get(&target.relation).cloned().ok_or_else(|| {
                SQLError::Internal(format!(
                    "foreign table `{}` has no loaded security metadata",
                    target.name
                ))
            })?;
            let columns = table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            validate_requested_columns(&target.relation, &columns, requested)?;
            Ok((target, security, columns))
        })
        .collect()
}

pub(super) fn table_privilege_updates(
    targets: Vec<(&ResolvedTableGrantTarget, Arc<TableState>)>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
) -> Result<Vec<TablePrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for (target, table) in targets {
        let current = table.security();
        let (next, grantable) = application.apply(&current)?;
        application.record_warning(grantable, &target.relation, notices);
        if next != current {
            updates.push((target.name.clone(), table, next));
        }
    }
    Ok(updates)
}

pub(super) fn view_privilege_updates(
    targets: Vec<(&ResolvedTableGrantTarget, StoredView)>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
) -> Result<Vec<ViewPrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for (target, mut view) in targets {
        let current = view.security();
        let (next, grantable) = application.apply(&current)?;
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{}` has no durable public column metadata",
                target.relation.qualified_name()
            ))
        })?;
        validate_table_security_invariants(&next, Some(columns), application.roles).map_err(
            |error| {
                SQLError::Internal(format!(
                    "view `{}` produced invalid privilege metadata: {error}",
                    target.relation.qualified_name()
                ))
            },
        )?;
        application.record_warning(grantable, &target.relation, notices);
        if next != current {
            view.set_security(next);
            updates.push((target.relation.clone(), view));
        }
    }
    Ok(updates)
}

pub(super) fn foreign_table_privilege_updates(
    targets: Vec<ForeignTableGrantTarget<'_>>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
) -> Result<Vec<ForeignTablePrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for (target, current, columns) in targets {
        let (next, grantable) = application.apply(&current)?;
        validate_table_security_invariants(&next, Some(&columns), application.roles).map_err(
            |error| {
                SQLError::Internal(format!(
                    "foreign table `{}` produced invalid privilege metadata: {error}",
                    target.relation.qualified_name()
                ))
            },
        )?;
        application.record_warning(grantable, &target.relation, notices);
        if next != current {
            updates.push((target.relation.clone(), next));
        }
    }
    Ok(updates)
}

pub(super) fn validate_requested_columns(
    target: &RelationIdentity,
    columns: &[String],
    requested: &RequestedTablePrivileges,
) -> Result<(), SQLError> {
    for (_, requested_column) in &requested.columns {
        if !columns.contains(requested_column) {
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{requested_column}\" of relation \"{}\" does not exist",
                    target.name
                ),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_table_grant_target_kinds(
    statement: &GrantTableStmt,
    targets: &[ResolvedTableGrantTarget],
) -> Result<(), SQLError> {
    for target in targets {
        if !matches!(
            target.kind,
            "table" | "view" | "materialized view" | "foreign table" | "sequence"
        ) {
            return Err(SQLError::Unsupported(format!(
                "{} privileges for \"{}\" are not supported",
                target.kind, target.requested
            )));
        }
    }
    if let Some(column) = statement
        .privileges
        .iter()
        .flat_map(|privilege| &privilege.columns)
        .next()
    {
        if let Some(target) = targets.iter().find(|target| target.kind == "sequence") {
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    target.relation.name
                ),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_table_acl_roles(
    statement: &GrantTableStmt,
    grantees: &[String],
    requested_grantor: Option<&str>,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role != "PUBLIC" && !roles.contains_key(role) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if statement.is_grant && statement.grant_option && grantees.iter().any(|role| role == "PUBLIC")
    {
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: "grant options can only be granted to roles".into(),
        });
    }
    if let Some(requested_grantor) = requested_grantor {
        if !roles.contains_key(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{requested_grantor}\" does not exist"),
            });
        }
        if requested_grantor != current_user {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

pub(super) fn table_sequence_privileges(
    privileges: &[uqa_sql::ast::TablePrivilegeSpec],
) -> (Vec<SequencePrivilege>, bool) {
    if privileges.is_empty() {
        return (
            vec![
                SequencePrivilege::Select,
                SequencePrivilege::Update,
                SequencePrivilege::Usage,
            ],
            false,
        );
    }
    let mut mapped = Vec::new();
    let mut inapplicable = false;
    for spec in privileges {
        let privilege = if spec.columns.is_empty() {
            match &spec.privilege {
                TablePrivilege::Select => Some(SequencePrivilege::Select),
                TablePrivilege::Update => Some(SequencePrivilege::Update),
                TablePrivilege::Usage => Some(SequencePrivilege::Usage),
                _ => {
                    inapplicable = true;
                    None
                }
            }
        } else {
            Some(SequencePrivilege::ColumnsUnsupported)
        };
        if let Some(privilege) = privilege {
            if !mapped.contains(&privilege) {
                mapped.push(privilege);
            }
        }
    }
    (mapped, inapplicable)
}

fn table_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}
