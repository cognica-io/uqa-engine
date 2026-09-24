//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename one selected role tuple while retaining its original authority and destination reservation.

use super::{context::RoleExecutionContext, tuples};
use crate::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use uqa_sql::{
    ast::RenameRoleStmt,
    catalog::roles::{rename as analysis, tuple::RoleTuple},
    SQLError,
};

pub fn rename_role(
    context: &RoleExecutionContext<'_>,
    statement: &RenameRoleStmt,
) -> Result<(), SQLError> {
    let (original, replacement) = {
        let roles = context.analysis.roles.role_definitions();
        let replacement = analysis::rename_candidate(&context.analysis, &roles, statement)?;
        (RoleTuple::bind(&roles[&statement.name])?, replacement)
    };
    tuples::lock(context, &original)?;
    let destination = context.locks.acquire_shared_catalog(
        SharedCatalogLock::Name {
            class_id: ROLE_CATALOG_CLASS_ID,
            name: &replacement.name,
        },
        RelationLockMode::AccessExclusive,
    )?;
    context.locks.refresh_shared_catalog()?;
    {
        let roles = context.analysis.roles.role_definitions();
        original.revalidate(&roles)?;
        analysis::require_available_name(&roles, &replacement.name, true)?;
    }
    destination.retain();
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    original.revalidate(&roles)?;
    analysis::require_available_name(&roles, &replacement.name, true)?;
    let mut next = roles.clone();
    next.remove(&original.role.name);
    next.insert(replacement.name.clone(), replacement);
    context.publication.persist_roles(&roles, &next)?;
    **roles = next;
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}
