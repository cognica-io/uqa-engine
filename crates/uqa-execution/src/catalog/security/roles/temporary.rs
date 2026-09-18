//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare shared visibility of committed temporary-object role dependencies.

use super::RoleCatalogGuards;
use crate::row_locks::{
    temporary_roles::{TemporaryRolePublication, MAX_TEMPORARY_ROLE_REFERENCES},
    RowLockManager,
};
use uqa_core::CancellationToken;
use uqa_sql::{
    catalog::roles::dependencies::{context::TemporaryRoleDependencyCatalog, temporary},
    SQLError,
};

pub trait TemporaryRoleDependencyReads {
    fn peer_temporary_role_reference(&self, oid: u32) -> Result<bool, SQLError>;
}

pub struct TemporaryRoleContext<'a> {
    pub catalog: &'a dyn TemporaryRoleDependencyCatalog,
    pub roles: &'a dyn RoleCatalogGuards,
    pub manager: &'a RowLockManager,
    pub session: u64,
    pub cancellation: &'a CancellationToken,
}

pub fn prepare_temporary_roles(
    context: TemporaryRoleContext<'_>,
) -> Result<Option<TemporaryRolePublication<'_>>, SQLError> {
    if !context.catalog.temporary_namespace_allocated()
        && !context
            .manager
            .has_temporary_role_references(context.session)
    {
        return Ok(None);
    }
    let referenced = {
        let roles = context.roles.role_definitions();
        temporary::role_dependencies(
            context.catalog,
            &roles,
            MAX_TEMPORARY_ROLE_REFERENCES as usize,
        )?
    };
    context
        .manager
        .prepare_temporary_roles(context.session, referenced, context.cancellation)
        .map(Some)
}
