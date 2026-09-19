//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove inherited constraints one level at a time while retaining each child's identity.

use super::{
    ddl_storage_error, publish_constraint_state, table_constraint_state, ConstraintAlterContext,
};
use crate::row_locks::{binding::lock_relation_identity, RelationLockMode};
use std::collections::BTreeSet;
use uqa_sql::{
    schema::constraint_changes::inheritance::{
        ensure_inherited_constraint_removable, inherited_constraint_removal, make_constraint_local,
        InheritedConstraint, InheritedConstraintKey, InheritedConstraintRemoval,
    },
    SQLError,
};

pub(super) fn drop_inherited_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    recurse: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    let Some(target) = InheritedConstraint::find(&columns, &constraints, name) else {
        return Ok(false);
    };
    let parents = if target.no_inherit {
        0
    } else {
        parent_count(context, table, target.key)?
    };
    ensure_inherited_constraint_removable(table, name, parents)?;
    drop_branch(
        context,
        table,
        target,
        recurse,
        cascade,
        &mut BTreeSet::new(),
    )?;
    Ok(true)
}

fn drop_branch(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    target: InheritedConstraint<'_>,
    recurse: bool,
    cascade: bool,
    visiting: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    if !visiting.insert(table.to_string()) {
        return Err(SQLError::Internal(format!(
            "constraint inheritance cycle reaches `{table}`"
        )));
    }
    let children = if target.no_inherit {
        Vec::new()
    } else {
        context
            .rows
            .partitions
            .catalog
            .direct_hierarchy_children(table)?
            .into_iter()
            .map(|name| {
                context
                    .lock_catalog
                    .relation_object_id(&name)
                    .map(|id| id.map(|id| (name, id)))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    super::drop::drop_constraint_one(context, table, target.name, false, cascade)?;
    for (name, identity) in children.into_iter().flatten() {
        let Some(child) = lock_relation_identity(
            context.lock_catalog,
            context.lock_session,
            name,
            identity,
            RelationLockMode::AccessExclusive,
            false,
        )?
        else {
            continue;
        };
        context
            .access
            .ensure_no_pending_events(&child, "ALTER TABLE")?;
        let (mut columns, mut constraints) = table_constraint_state(context, &child)?;
        let inherited = target.key.require(&child, &columns, &constraints)?;
        let remaining = parent_count(context, &child, inherited.key)?;
        match inherited_constraint_removal(recurse, inherited.is_local, remaining) {
            InheritedConstraintRemoval::Drop => {
                context.access.ensure_table_owner(&child)?;
                drop_branch(context, &child, inherited, true, cascade, visiting)?;
            }
            InheritedConstraintRemoval::MakeLocal => {
                let name = inherited.name.to_string();
                make_constraint_local(&mut columns, &mut constraints, &name)?;
                publish_constraint_state(context, &child, columns, constraints)?;
            }
            InheritedConstraintRemoval::Keep => {}
        }
    }
    visiting.remove(table);
    Ok(())
}

pub(super) fn parent_count(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key: InheritedConstraintKey<'_>,
) -> Result<usize, SQLError> {
    let parents = context
        .relations
        .table_hierarchy(table)
        .map_err(|error| ddl_storage_error("read constraint inheritance", error))?
        .parents;
    let mut count = 0;
    for parent in parents {
        let (columns, constraints) = table_constraint_state(context, &parent)?;
        if key
            .find(&columns, &constraints)
            .is_some_and(|target| !target.no_inherit)
        {
            count += 1;
        }
    }
    Ok(count)
}
