//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind GRANT roots before writer admission and serialize ACL updates by catalog tuple.

use super::{authority, context::TableGrantContext, targets::system_target};
use crate::row_locks::{
    binding::{acquire_relation, bind_relation, RelationBinding},
    LockAcquire, RelationLockMode,
};
use uqa_sql::{
    ast::{GrantTableStmt, LockStrength, LockWait},
    catalog::security::table_grants::{
        targets::bind_named_table_grants, validate_table_grant_target_kinds,
        ResolvedTableGrantTarget,
    },
    SQLError,
};

pub(super) fn lock_targets(
    context: &TableGrantContext<'_>,
    statement: &GrantTableStmt,
    command_roles: &mut uqa_sql::catalog::security::acl_command::AclCommandRoles,
) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
    let targets = context.resolve_table_grant_targets(&statement.target)?;
    let mut bound = Vec::with_capacity(targets.len());
    for target in targets {
        let mut current = bind_relation(
            context.locks,
            RelationLockMode::AccessShare,
            false,
            || {
                let mut candidates = bind_named_table_grants(
                    context.resolution,
                    std::slice::from_ref(&target.requested),
                )?;
                let value = candidates.remove(0);
                Ok(Some(RelationBinding {
                    object_id: context.bindings.relation_object_id(&value.name)?,
                    name: value.name.clone(),
                    value,
                }))
            },
            |binding| {
                validate_table_grant_target_kinds(statement, std::slice::from_ref(&binding.value))
            },
        )?
        .ok_or_else(|| SQLError::Internal("GRANT binding unexpectedly disappeared".into()))?;
        if current.value.kind != "sequence" {
            {
                let roles = context.roles.role_definitions();
                super::prepared::resolve_roles(context, statement, command_roles, &roles)?;
            }
            let oid = if let Some(system) = system_target(&current.value) {
                system.oid()
            } else {
                let identity = current.object_id.ok_or_else(|| {
                    SQLError::Internal("GRANT target has no bound identity".into())
                })?;
                uqa_sql::catalog::oids::stable_object_oid("relation", &identity)
            };
            for catalog in ["pg_catalog.pg_class", "pg_catalog.pg_attribute"] {
                acquire_relation(
                    context.locks,
                    catalog,
                    RelationLockMode::RowExclusive,
                    false,
                )?
                .retain();
            }
            context.locks.refresh_after_wait()?;
            let requested =
                uqa_sql::catalog::security::table::requested_acl_privileges(&statement.privileges)?;
            let (columns, _) = authority::security(context, &current.value)?;
            uqa_sql::catalog::security::table_grants::validate_requested_columns(
                &current.value.relation,
                &columns,
                &requested,
            )?;
            if !requested.table.is_empty() {
                lock_tuple(context, &current.value, oid, None, 0)?;
            }
            let mut selected = std::collections::BTreeSet::new();
            for (index, column) in columns.iter().enumerate() {
                if authority::replaces_attribute(
                    context,
                    statement,
                    &current.value,
                    column,
                    command_roles,
                )? {
                    lock_tuple(context, &current.value, oid, Some(column), index as u64 + 1)?;
                    selected.insert(column.clone());
                }
            }
            current.value.acl_columns = Some(selected);
        }
        bound.push(current.value);
    }
    Ok(bound)
}

fn lock_tuple(
    context: &TableGrantContext<'_>,
    target: &ResolvedTableGrantTarget,
    oid: i64,
    column: Option<&str>,
    attribute: u64,
) -> Result<(), SQLError> {
    let before = authority::revision(context, target, column)?;
    let (catalog, doc_id) = if column.is_some() {
        ("pg_catalog.pg_attribute", ((oid as u64) << 16) | attribute)
    } else {
        ("pg_catalog.pg_class", oid as u64)
    };
    match context.rows.lock_row(
        catalog,
        doc_id,
        LockStrength::ForNoKeyUpdate,
        LockWait::Block,
        &target.relation.qualified_name(),
    )? {
        LockAcquire::Granted { .. } => {}
        LockAcquire::Skipped => {
            return Err(SQLError::Internal(
                "blocking ACL tuple lock was skipped".into(),
            ))
        }
    }
    context.locks.refresh_after_wait()?;
    if authority::revision(context, target, column)? != before {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: "tuple concurrently updated".into(),
        });
    }
    Ok(())
}
