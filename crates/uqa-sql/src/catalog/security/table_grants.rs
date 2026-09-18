//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table and column GRANT/REVOKE validation, ACL candidates and diagnostics.
#[cfg(test)]
mod tests;
use super::{
    columns::{grant_column_acl, revoke_column_acl, select_column_acl_grantor},
    table::{
        grant_acl, revoke_acl, select_acl_grantor, validate_table_security_invariants,
        RequestedTablePrivileges, TableAclPrivilege,
    },
    BoundTableSecurity, TableSecurity,
};
use crate::catalog::roles::identity::RoleSubject;
use crate::catalog::{
    roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
    stored_view::StoredView,
};
use crate::{
    ast::{GrantTableStmt, SequencePrivilege, TablePrivilege, TableRevokeBehavior},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::catalog_acl::AclGrantee;
use uqa_core::RelationIdentity;
pub type ViewPrivilegeUpdate = (RelationIdentity, StoredView);
pub type ForeignTablePrivilegeUpdate = (RelationIdentity, BoundTableSecurity);
pub type ForeignTableGrantTarget<'a> = (
    &'a ResolvedTableGrantTarget,
    BoundTableSecurity,
    Vec<String>,
);
pub mod targets;
pub struct ResolvedTableGrantTarget {
    pub requested: String,
    pub name: String,
    pub relation: RelationIdentity,
    pub kind: &'static str,
}

fn apply_table_acl(
    statement: &GrantTableStmt,
    grantees: &[AclGrantee],
    privileges: &[TableAclPrivilege],
    current_user: &(impl RoleSubject + ?Sized),
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
    application: &TableGrantApplication<'_>,
    privileges: &[(TableAclPrivilege, String)],
    authorization: &TableSecurity,
    current: &TableSecurity,
) -> Result<(TableSecurity, usize), SQLError> {
    let TableGrantApplication {
        statement,
        grantees,
        current_user,
        roles,
        memberships,
        ..
    } = application;
    let grantors = privileges
        .iter()
        .map(|(privilege, column)| {
            (
                *privilege,
                column.clone(),
                select_column_acl_grantor(
                    authorization,
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
        .filter(|(privilege, column, grantor)| {
            grantor.is_some()
                && application
                    .requested
                    .columns
                    .contains(&(*privilege, column.clone()))
        })
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

pub struct TableGrantApplication<'a> {
    pub statement: &'a GrantTableStmt,
    pub grantees: &'a [AclGrantee],
    pub requested: &'a RequestedTablePrivileges,
    pub current_user: &'a dyn RoleSubject,
    pub roles: &'a BTreeMap<String, RoleDefinition>,
    pub memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
}

impl TableGrantApplication<'_> {
    pub fn apply(&self, current: &TableSecurity) -> Result<(TableSecurity, usize), SQLError> {
        let (next, table_grantable) = apply_table_acl(
            self.statement,
            self.grantees,
            &self.requested.table,
            self.current_user,
            self.roles,
            self.memberships,
            current,
        )?;
        let mut columns = self.requested.columns.clone();
        let mut implied = Vec::new();
        if !self.statement.is_grant {
            for privilege in &self.requested.table {
                if matches!(
                    privilege,
                    TableAclPrivilege::Select
                        | TableAclPrivilege::Insert
                        | TableAclPrivilege::Update
                        | TableAclPrivilege::References
                ) {
                    for column in current.column_acls.keys() {
                        let key = (*privilege, column.clone());
                        if !columns.contains(&key) {
                            columns.push(key.clone());
                        }
                        implied.push(key);
                    }
                }
            }
        }
        let (mut next, column_grantable) = apply_column_acl(self, &columns, current, &next)?;
        // Relation grant-option loss also invalidates column grants made through that relation authority.
        for (privilege, column) in implied {
            let before = super::columns::column_grant_option_roles(current, &column, privilege);
            super::columns::revoke_dependent_column_acl(
                &mut next,
                &column,
                privilege,
                &before,
                self.statement.revoke_behavior == TableRevokeBehavior::Cascade,
            )?;
        }
        next.column_acls.retain(|_, acl| !acl.is_empty());
        Ok((next, table_grantable + column_grantable))
    }

    pub fn record_warning(
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

pub fn validate_requested_columns(
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

pub fn validate_table_grant_target_kinds(
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

pub fn validate_table_acl_roles(
    statement: &GrantTableStmt,
    grantees: &[AclGrantee],
    requested_grantor: Option<&str>,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role
            .role_name()
            .is_some_and(|name| !roles.contains_key(name))
        {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if statement.is_grant && statement.grant_option && grantees.iter().any(AclGrantee::is_public) {
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
        if current_user.role_name(roles) != Some(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

pub fn table_sequence_privileges(
    privileges: &[crate::ast::TablePrivilegeSpec],
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
pub fn view_privilege_updates(
    targets: Vec<(&ResolvedTableGrantTarget, StoredView)>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
    dependencies: &mut std::collections::BTreeSet<String>,
) -> Result<Vec<ViewPrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for (target, mut view) in targets {
        let current = view
            .security
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let (next, grantable) = application.apply(&current)?;
        crate::catalog::security::dependencies::added_table_acl_roles(
            &current,
            &next,
            dependencies,
        );
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
            view.set_security(
                BoundTableSecurity::bind(&next, application.roles).map_err(SQLError::Internal)?,
            );
            updates.push((target.relation.clone(), view));
        }
    }
    Ok(updates)
}
pub fn foreign_table_privilege_updates(
    targets: Vec<ForeignTableGrantTarget<'_>>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
    dependencies: &mut std::collections::BTreeSet<String>,
) -> Result<Vec<ForeignTablePrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for (target, current, columns) in targets {
        let current = current
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let (next, grantable) = application.apply(&current)?;
        crate::catalog::security::dependencies::added_table_acl_roles(
            &current,
            &next,
            dependencies,
        );
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
            updates.push((
                target.relation.clone(),
                BoundTableSecurity::bind(&next, application.roles).map_err(SQLError::Internal)?,
            ));
        }
    }
    Ok(updates)
}
