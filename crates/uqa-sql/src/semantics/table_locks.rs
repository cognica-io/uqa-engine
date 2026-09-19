//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit relation-lock authorization and stored-view target scope.

use crate::catalog::roles::RoleReference;
use crate::{
    ast::{LockTableTarget, TableLockMode},
    catalog::{
        roles::guards::RoleCatalogGuards,
        security::{
            table::{role_has_table_privilege, TableAclPrivilege},
            BoundTableSecurity,
        },
    },
    plan::QueryPlan,
    SQLError,
};

pub fn ensure_lock_privilege(
    roles: &dyn RoleCatalogGuards,
    security: &BoundTableSecurity,
    subject: &RoleReference,
    mode: TableLockMode,
    name: &str,
    kind: &str,
) -> Result<(), SQLError> {
    let definitions = roles.role_definitions();
    let memberships = roles.role_memberships();
    let security = security.resolve(&definitions).map_err(SQLError::Internal)?;
    let system = crate::catalog::SystemRelation::from_qualified_name(name);
    if TableAclPrivilege::ALL.into_iter().any(|privilege| {
        lock_privilege_permits(privilege, mode)
            && system.map_or_else(
                || {
                    role_has_table_privilege(
                        &security,
                        subject,
                        privilege,
                        &definitions,
                        &memberships,
                    )
                },
                |relation| {
                    crate::catalog::security::system_relations::has_table_privilege(
                        relation,
                        &security,
                        subject,
                        crate::catalog::security::table::TablePrivilegeCheck {
                            privilege,
                            grant_option: false,
                        },
                        &definitions,
                        &memberships,
                    )
                },
            )
    }) {
        return Ok(());
    }
    let (_, local) =
        uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("permission denied for {kind} {local}"),
    })
}

pub fn lock_privilege_permits(privilege: TableAclPrivilege, mode: TableLockMode) -> bool {
    match privilege {
        TableAclPrivilege::Maintain
        | TableAclPrivilege::Update
        | TableAclPrivilege::Delete
        | TableAclPrivilege::Truncate => true,
        TableAclPrivilege::Select => mode == TableLockMode::AccessShare,
        TableAclPrivilege::Insert => matches!(
            mode,
            TableLockMode::AccessShare | TableLockMode::RowShare | TableLockMode::RowExclusive
        ),
        TableAclPrivilege::References | TableAclPrivilege::Trigger => false,
    }
}

pub fn view_lock_targets(query: &QueryPlan) -> Result<Vec<LockTableTarget>, SQLError> {
    let mut targets = Vec::new();
    crate::binding::view_dependencies::bind_query_plan_relation_targets(
        &mut query.clone(),
        &std::collections::BTreeSet::new(),
        &mut |name, include_descendants| {
            targets.push(LockTableTarget {
                name: name.into(),
                include_descendants,
            });
            Ok::<_, SQLError>(name.into())
        },
    )?;
    Ok(targets)
}

#[cfg(test)]
mod tests;
