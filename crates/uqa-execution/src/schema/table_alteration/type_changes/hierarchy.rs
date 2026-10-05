//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain each inheritor once before preparing its original row descriptor.

use super::super::TableAlterContext;
use crate::row_locks::{binding::lock_relation_identity, RelationLockMode};
use std::collections::BTreeSet;
use uqa_sql::{ast::AlterTableAction, schema::columns::type_target, SQLError};

pub(super) fn lock_relations<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    root: &str,
    recurse: bool,
    mode: RelationLockMode,
    actions: &[AlterTableAction],
) -> Result<Vec<String>, SQLError> {
    let catalog = context.hierarchy.partitions.catalog;
    let mut relations = vec![root.to_string()];
    if !recurse {
        if !catalog.direct_hierarchy_children(root)?.is_empty() {
            if let Some(AlterTableAction::AlterColumnType { name, .. }) = actions
                .iter()
                .find(|action| matches!(action, AlterTableAction::AlterColumnType { .. }))
            {
                return Err(type_target::require_type_change_descendants(name));
            }
        }
        return Ok(relations);
    }
    let mut seen = BTreeSet::new();
    if let Some(id) = context.binding.catalog.relation_object_id(root)? {
        seen.insert(id);
    }
    let mut position = 0;
    while position < relations.len() {
        // Read children after acquiring their parent's lock and refreshing its catalog.
        let mut children = catalog
            .direct_hierarchy_children(&relations[position])?
            .into_iter()
            .map(|name| {
                let oid = context.binding.catalog.relation_catalog_oid(&name)?;
                Ok((oid, name))
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        children.sort();
        for (_, name) in children {
            let Some(id) = context.binding.catalog.relation_object_id(&name)? else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            if let Some(current) = lock_relation_identity(
                context.binding.catalog,
                context.binding.locks,
                name,
                id,
                mode,
                false,
            )? {
                context.constraints.access.ensure_table_owner(&current)?;
                relations.push(current);
            }
        }
        position += 1;
    }
    Ok(relations)
}

pub(super) fn validate_parents<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    relations: &[String],
    actions: &[AlterTableAction],
) -> Result<(), SQLError> {
    let hierarchy = context
        .hierarchy
        .partitions
        .catalog
        .try_table_hierarchy(table)
        .map_err(SQLError::Internal)?;
    for action in actions {
        let AlterTableAction::AlterColumnType { name, .. } = action else {
            continue;
        };
        for parent in &hierarchy.parents {
            if !relations.contains(parent)
                && context
                    .addition
                    .state
                    .has_column(parent, name)
                    .map_err(|error| super::ddl_storage_error("ALTER COLUMN TYPE", error))?
            {
                return Err(type_target::reject_external_type_parent(table, name));
            }
        }
    }
    Ok(())
}
