//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Drop binds every role before waiting and checks dependencies after exclusion.

use super::context::RoleExecutionContext;
use crate::{
    catalog::security::roles::locking::{RoleBinding, RoleLockContext, ROLE_CATALOG_CLASS_ID},
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{ast::DropRoleStmt, catalog::roles::definition, SQLError};

pub(super) fn lock_drop_targets(
    context: &RoleExecutionContext<'_>,
    statement: &DropRoleStmt,
    current: &RoleReference,
    session: &RoleReference,
) -> Result<Vec<String>, SQLError> {
    let snapshot = context.analysis.roles.role_definitions().clone();
    let names = definition::resolve_drop_role_names(
        &context.analysis,
        statement,
        current,
        session,
        &snapshot,
    )?;
    let bindings = names
        .iter()
        .map(|name| RoleBinding::from_definition(&snapshot[name]))
        .collect::<Result<Vec<_>, _>>()?;
    let role_locks = RoleLockContext {
        roles: context.analysis.roles,
        session: context.locks,
    };
    for bound in bindings {
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: ROLE_CATALOG_CLASS_ID,
                oid: bound.oid,
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.locks.refresh_shared_catalog()?;
        if role_locks.revalidate(&bound).is_err() {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("could not find tuple for role {}", bound.oid),
            });
        }
        guard.retain();
    }
    Ok(names)
}
