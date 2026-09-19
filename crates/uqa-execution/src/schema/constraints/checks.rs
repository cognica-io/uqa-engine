//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute recursive CHECK merging and renaming.
use super::{
    constraint_error, ddl_storage_error, ensure_constraint_name_available, find_constraint,
    publish_constraint_state, table_constraint_state, validate_check_expression,
    ConstraintAlterContext, ConstraintLocation, SQLError,
};
use std::collections::BTreeSet;
use uqa_sql::ast::{TableCheck, TableLockMode};

fn find_check(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<Option<TableCheck>, SQLError> {
    Ok(context
        .catalog
        .try_check_constraint_definitions(table)
        .map_err(|error| ddl_storage_error("read CHECK constraints", error))?
        .into_iter()
        .find(|check| check.name.as_deref() == Some(name)))
}

fn replace_check(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    from: &str,
    check: TableCheck,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    match find_constraint(&columns, &constraints, from) {
        Some(ConstraintLocation::ColumnCheck(index)) => {
            let column = &mut columns[index];
            column.check = Some(check.expr);
            column.check_name = check.name;
            column.check_object_id = check.object_id;
            column.check_is_local = check.is_local;
            column.check_enforced = check.enforced;
            column.check_validated = check.validated;
            column.check_no_inherit = check.no_inherit;
        }
        Some(ConstraintLocation::TableCheck(index)) => constraints.checks[index] = check,
        _ => {
            return Err(SQLError::Internal(format!(
                "CHECK constraint `{from}` disappeared"
            )))
        }
    }
    publish_constraint_state(context, table, columns, constraints)
}

pub fn merge_added_check(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    mut incoming: TableCheck,
) -> Result<bool, SQLError> {
    let Some(name) = incoming.name.clone() else {
        return Ok(false);
    };
    let Some(mut existing) = find_check(context, table, &name)? else {
        return Ok(false);
    };
    let (columns, constraints) = table_constraint_state(context, table)?;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    validate_check_expression(context, table, &relation.name, &columns, &mut incoming.expr)?;
    uqa_sql::catalog::regrole_dependencies::reject_stored_regrole_constants(
        context.publication.bindings.schema,
        &incoming.expr,
        None,
    )?;
    if incoming.is_local && (existing.is_local || constraints.hierarchy.is_partition()) {
        return Err(uqa_sql::schema::check_inheritance::duplicate_check(
            &relation.name,
            &name,
        ));
    }
    uqa_sql::schema::check_inheritance::validate_check_merge(
        &relation.name,
        &existing,
        &incoming,
        &columns,
    )?;
    let was_local = existing.is_local;
    let was_enforced = existing.enforced;
    existing.is_local |= incoming.is_local;
    if constraints.hierarchy.is_partition() {
        existing.is_local = false;
    }
    if incoming.enforced && !existing.enforced {
        // PostgreSQL's inherited ADD CHECK merge marks an enforcement upgrade valid without scanning existing rows.
        existing.enforced = true;
        existing.validated = true;
    }
    if existing.is_local != was_local || existing.enforced != was_enforced {
        replace_check(context, table, &name, existing)?;
    }
    context.notices.lock().push((
        "NOTICE".to_string(),
        format!("merging constraint \"{name}\" with inherited definition"),
    ));
    Ok(true)
}

pub fn rename_check(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    from: &str,
    to: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    let Some(check) = find_check(context, table, from)? else {
        return Ok(false);
    };
    if !check.no_inherit
        && !recurse
        && !context
            .rows
            .partitions
            .catalog
            .direct_hierarchy_children(table)?
            .is_empty()
    {
        return Err(constraint_error(
            "42P16",
            format!("inherited constraint \"{from}\" must be renamed in child tables too"),
        ));
    }
    let mut targets = if check.no_inherit || !recurse {
        vec![table.to_string()]
    } else {
        context.rows.catalog.hierarchy_scan_tables(table, true)?
    };
    // PostgreSQL checks descendants before the directly named constraint, so child ownership and name conflicts precede a root inheritance error.
    targets.retain(|target| target != table);
    targets.push(table.to_string());
    let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for target in targets {
        context
            .locks
            .lock_relation(&target, TableLockMode::AccessExclusive)?;
        context.access.ensure_table_owner(&target)?;
        let (columns, constraints) = table_constraint_state(context, &target)?;
        let mut check = find_check(context, &target, from)?.ok_or_else(|| {
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
        if !check.no_inherit && parent_count(context, &target, from)? > expected {
            return Err(constraint_error(
                "42P16",
                format!("cannot rename inherited constraint \"{from}\""),
            ));
        }
        ensure_constraint_name_available(&columns, &constraints, Some(to), &target)?;
        check.name = Some(to.to_string());
        changes.push((target, check));
    }
    for (target, check) in changes {
        replace_check(context, &target, from, check)?;
    }
    Ok(true)
}

fn parent_count(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<usize, SQLError> {
    super::inheritance::parent_count(
        context,
        table,
        uqa_sql::schema::constraint_changes::inheritance::InheritedConstraintKey::Check(name),
    )
}
