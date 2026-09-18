//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind GRANT roots before writer admission and serialize system ACL updates by catalog tuple.

use super::{context::TableGrantContext, targets::system_target};
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
) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
    let targets = context.resolve_table_grant_targets(&statement.target)?;
    let mut bound = Vec::with_capacity(targets.len());
    for target in targets {
        let current = bind_relation(
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
        if let Some(system) = system_target(&current.value) {
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
            let columns = system.column_names();
            uqa_sql::catalog::security::table_grants::validate_requested_columns(
                &current.value.relation,
                &columns,
                &requested,
            )?;
            if !requested.table.is_empty() {
                lock_tuple(context, system, None, 0)?;
            }
            let current_security = context.system.system_relation_security(system);
            for (index, column) in columns.iter().enumerate() {
                if requested.columns.iter().any(|(_, name)| name == column)
                    || (!statement.is_grant
                        && !requested.table.is_empty()
                        && current_security.column_acls.contains_key(column))
                {
                    lock_tuple(context, system, Some(column), index as u64 + 1)?;
                }
            }
        }
        bound.push(current.value);
    }
    Ok(bound)
}

fn lock_tuple(
    context: &TableGrantContext<'_>,
    relation: uqa_sql::catalog::SystemRelation,
    column: Option<&str>,
    attribute: u64,
) -> Result<(), SQLError> {
    use super::super::system_relations::tuple_revision;
    let before = tuple_revision(context.system, relation, column);
    let (catalog, doc_id) = if column.is_some() {
        (
            "pg_catalog.pg_attribute",
            ((relation.oid() as u64) << 16) | attribute,
        )
    } else {
        ("pg_catalog.pg_class", relation.oid() as u64)
    };
    match context.rows.lock_row(
        catalog,
        doc_id,
        LockStrength::ForNoKeyUpdate,
        LockWait::Block,
        &relation.qualified_name(),
    )? {
        LockAcquire::Granted { .. } => {}
        LockAcquire::Skipped => {
            return Err(SQLError::Internal(
                "blocking system ACL tuple lock was skipped".into(),
            ))
        }
    }
    context.locks.refresh_after_wait()?;
    if tuple_revision(context.system, relation, column) != before {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: "tuple concurrently updated".into(),
        });
    }
    Ok(())
}
