//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View ownership, schema-owner drop authority, and materialized-view maintenance rules.
use crate::{
    catalog::{
        roles::{guards::RoleCatalogGuards, role_inherits, RoleReferenceNames},
        security::table::{role_has_table_privilege, TableAclPrivilege},
        stored_view::StoredView,
        view::StoredViewKind,
    },
    SQLError,
};
use uqa_core::RelationIdentity;

pub use super::ownership::RelationOwnerSchemas as ViewOwnerSchemas;
#[derive(Clone, Copy)]
pub struct ViewOwnershipContext<'a> {
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub schemas: &'a dyn ViewOwnerSchemas,
}
fn current_user_has_role_privileges(context: ViewOwnershipContext<'_>, target: &str) -> bool {
    let current = context.session.current_user_name();
    let roles = context.roles.role_definitions();
    let memberships = context.roles.role_memberships();
    role_inherits(&roles, &memberships, &current, target)
}

fn view_kind_name(view: &StoredView) -> &'static str {
    match view.kind {
        StoredViewKind::View => "view",
        StoredViewKind::Materialized => "materialized view",
    }
}

pub fn ensure_view_owner(
    context: ViewOwnershipContext<'_>,
    canonical_name: &str,
    view: &StoredView,
) -> Result<String, SQLError> {
    if current_user_has_role_privileges(context, &view.role_owner) {
        return Ok(view.role_owner.clone());
    }
    let relation = RelationIdentity::from_legacy_name(canonical_name)
        .map_err(|error| SQLError::Internal(format!("resolve view `{canonical_name}`: {error}")))?;
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!(
            "must be owner of {} {}",
            view_kind_name(view),
            relation.name
        ),
    })
}

pub fn ensure_view_drop_authority(
    context: ViewOwnershipContext<'_>,
    canonical_name: &str,
    view: &StoredView,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(canonical_name)
        .map_err(|error| SQLError::Internal(format!("resolve view `{canonical_name}`: {error}")))?;
    if current_user_has_role_privileges(context, &view.role_owner)
        || context
            .schemas
            .schema_security(&relation.schema)
            .is_some_and(|security| current_user_has_role_privileges(context, &security.role_owner))
    {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!(
            "must be owner of {} {}",
            view_kind_name(view),
            relation.name
        ),
    })
}

pub fn ensure_materialized_view_maintenance(
    context: ViewOwnershipContext<'_>,
    canonical_name: &str,
    view: &StoredView,
) -> Result<(), SQLError> {
    debug_assert_eq!(view.kind, StoredViewKind::Materialized);
    let current_user = context.session.current_user_name();
    let roles = context.roles.role_definitions();
    let memberships = context.roles.role_memberships();
    if role_has_table_privilege(
        &view.security(),
        &current_user,
        TableAclPrivilege::Maintain,
        &roles,
        &memberships,
    ) {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve materialized view `{canonical_name}`: {error}"
        ))
    })?;
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("permission denied for materialized view {}", relation.name),
    })
}
#[cfg(test)]
mod tests;
