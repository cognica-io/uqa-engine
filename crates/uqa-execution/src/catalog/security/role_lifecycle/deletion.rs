//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role deletion follows requested target order through exclusion, membership removal and tuple deletion.

use super::{context::RoleExecutionContext, tuples};
use crate::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use uqa_sql::{
    ast::DropRoleStmt,
    catalog::roles::{
        definition::{role_drop_memberships_candidate, RoleDropAuthority},
        identity::RoleBinding,
    },
    SQLError,
};

pub fn drop_roles(
    context: &RoleExecutionContext<'_>,
    statement: &DropRoleStmt,
) -> Result<(), SQLError> {
    let current = context.analysis.names.current_role();
    let session = context.analysis.names.session_role();
    let authority =
        RoleDropAuthority::new(current, session, &context.analysis.roles.role_definitions())?;
    let mut targets: Vec<RoleBinding> = Vec::new();
    for requested in &statement.names {
        let roles = context.analysis.roles.role_definitions().clone();
        let Some(target) =
            authority.resolve_target(&context.analysis, requested, statement.if_exists, &roles)?
        else {
            continue;
        };
        drop(roles);
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: ROLE_CATALOG_CLASS_ID,
                oid: target.oid,
            },
            RelationLockMode::AccessExclusive,
        )?;
        guard.retain();
        context.locks.refresh_shared_catalog()?;
        remove_memberships(context, &target)?;
        if !targets
            .iter()
            .any(|old| old.identity() == target.identity())
        {
            targets.push(target);
        }
    }
    for target in targets {
        let tuple = tuples::prepare_drop(context, &target)?;
        tuples::lock(context, &tuple)?;
        let mut roles = context.registry.write_roles();
        let name = tuple.revalidate(&roles)?.name.clone();
        let mut next = roles.clone();
        next.remove(&name);
        context.publication.persist_roles(&roles, &next)?;
        **roles = next;
        drop(roles);
        context.publication.catalog_changed();
    }
    Ok(())
}

fn remove_memberships(
    context: &RoleExecutionContext<'_>,
    target: &RoleBinding,
) -> Result<(), SQLError> {
    context.publication.prepare_writer()?;
    let roles = context.analysis.roles.role_definitions();
    let mut memberships = context.registry.write_memberships();
    let next = role_drop_memberships_candidate(&memberships, target.identity());
    context
        .publication
        .persist_memberships(&memberships, &next)?;
    let changed = **memberships != next;
    if changed {
        **memberships = next;
    }
    drop(memberships);
    drop(roles);
    if changed {
        context.publication.catalog_changed();
    }
    Ok(())
}
