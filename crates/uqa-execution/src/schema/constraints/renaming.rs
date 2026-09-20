//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename local foreign keys and inheritable CHECK/NOT NULL constraints with their original identities.

use super::{
    constraint_error, ensure_constraint_name_available, publish_constraint_state,
    table_constraint_state, ConstraintAlterContext,
};
use std::collections::BTreeSet;
use uqa_sql::{
    ast::TableLockMode,
    schema::constraint_changes::{
        inheritance::InheritedConstraint,
        renaming::{
            ensure_recursive_rename, ensure_rename_parents, rename_foreign_key,
            rename_inherited_constraint,
        },
    },
    SQLError,
};

pub fn rename_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    from: &str,
    to: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    if let Some(position) = constraints
        .key_constraints
        .iter()
        .position(|key| key.name.as_deref() == Some(from))
    {
        rename_key_constraint(context, table, to, position, columns, constraints)?;
        return Ok(true);
    }
    if rename_foreign_key(table, &mut columns, &mut constraints, from, to)? {
        publish_constraint_state(context, table, columns, constraints)?;
        return Ok(true);
    }
    let Some(root) = InheritedConstraint::find(&columns, &constraints, from) else {
        return Ok(false);
    };
    if !root.no_inherit {
        ensure_recursive_rename(
            from,
            recurse,
            !context
                .rows
                .partitions
                .catalog
                .direct_hierarchy_children(table)?
                .is_empty(),
        )?;
    }
    let mut targets = if root.no_inherit || !recurse {
        vec![table.to_string()]
    } else {
        context.rows.catalog.hierarchy_scan_tables(table, true)?
    };
    // PostgreSQL checks descendant ownership and conflicts before the root inheritance error.
    targets.retain(|target| target != table);
    targets.push(table.to_string());
    let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
    for target in &targets {
        context
            .locks
            .lock_relation(target, TableLockMode::AccessExclusive)?;
        context.access.ensure_table_owner(target)?;
        let (columns, constraints) = table_constraint_state(context, target)?;
        let constraint =
            InheritedConstraint::find(&columns, &constraints, from).ok_or_else(|| {
                constraint_error(
                    "42704",
                    format!("constraint \"{from}\" for table \"{target}\" does not exist"),
                )
            })?;
        let expected = if target == table {
            0
        } else {
            constraints
                .hierarchy
                .parents
                .iter()
                .filter(|parent| target_set.contains(*parent))
                .count()
        };
        if !constraint.no_inherit {
            ensure_rename_parents(
                from,
                super::inheritance::parent_count(context, target, constraint.key)?,
                expected,
            )?;
        }
        ensure_constraint_name_available(&columns, &constraints, Some(to), target)?;
    }
    for target in targets {
        let (mut columns, mut constraints) = table_constraint_state(context, &target)?;
        rename_inherited_constraint(&target, &mut columns, &mut constraints, from, to)?;
        publish_constraint_state(context, &target, columns, constraints)?;
    }
    Ok(true)
}

fn rename_key_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    to: &str,
    position: usize,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    mut constraints: uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    ensure_constraint_name_available(&columns, &constraints, Some(to), table)?;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    if !context.names.relation_name_available(
        &uqa_core::RelationIdentity::new(&relation.schema, to).qualified_name(),
    )? {
        return Err(constraint_error(
            "42P07",
            format!("relation \"{to}\" already exists"),
        ));
    }
    let key = &mut constraints.key_constraints[position];
    key.name = Some(to.into());
    let identity = key.catalog_identity;
    for inherited in &mut constraints.hierarchy.partition_inherited_key_constraints {
        if inherited.catalog_identity == identity {
            inherited.name = Some(to.into());
        }
    }
    let owner = identity
        .ok_or_else(|| SQLError::Internal("key constraint has no catalog identity".into()))?
        .object_id;
    crate::schema::indexes::renaming::rename_owned_constraint(
        &context.publication.indexes,
        table,
        owner,
        to,
        columns,
        constraints,
    )
    .map_err(|error| uqa_sql::catalog::errors::storage_error("RENAME CONSTRAINT", &error))?;
    Ok(())
}
