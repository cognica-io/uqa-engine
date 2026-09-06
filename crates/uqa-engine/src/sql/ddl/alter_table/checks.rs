//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK origin and identity during recursive constraint changes.

use super::{
    constraint_error, ddl_storage_error, ensure_constraint_name_available, find_constraint,
    publish_constraint_state, table_constraint_state, ConstraintLocation, Engine, SQLError,
};
use std::collections::BTreeSet;
use uqa_sql::ast::TableCheck;

pub(super) fn take_column_check(column: &mut uqa_sql::ast::ColumnDef) -> Option<TableCheck> {
    let check = TableCheck {
        expr: column.check.take()?,
        name: column.check_name.take(),
        object_id: column.check_object_id.take(),
        is_local: column.check_is_local,
        enforced: column.check_enforced,
        validated: column.check_validated,
        no_inherit: column.check_no_inherit,
        partition_constraint: None,
    };
    column.check_is_local = true;
    column.check_enforced = true;
    column.check_validated = true;
    column.check_no_inherit = false;
    Some(check)
}

fn find_check(engine: &Engine, table: &str, name: &str) -> Result<Option<TableCheck>, SQLError> {
    Ok(engine
        .try_check_constraint_definitions(table)
        .map_err(|error| ddl_storage_error("read CHECK constraints", error))?
        .into_iter()
        .find(|check| check.name.as_deref() == Some(name)))
}

fn parent_count(engine: &Engine, table: &str, name: &str) -> Result<usize, SQLError> {
    engine
        .try_check_constraint_parent_count(table, name)
        .map_err(|error| ddl_storage_error("read CHECK inheritance", error))
}

fn replace_check(
    engine: &Engine,
    table: &str,
    from: &str,
    check: TableCheck,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
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
    publish_constraint_state(engine, table, columns, constraints)
}

pub(super) fn validate_check(
    engine: &Engine,
    table: &str,
    name: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    let Some(check) = find_check(engine, table, name)? else {
        return Ok(false);
    };
    if !check.enforced {
        return Err(constraint_error(
            "55000",
            "cannot validate NOT ENFORCED constraint",
        ));
    }
    if check.validated {
        return Ok(true);
    }
    if !check.no_inherit {
        let targets = engine.hierarchy_scan_tables(table, true)?;
        if !recurse && targets.len() > 1 {
            return Err(constraint_error(
                "42P16",
                "constraint must be validated on child tables too",
            ));
        }
        for child in targets.iter().filter(|target| target.as_str() != table) {
            engine.lock_relation(child, crate::row_locks::RelationLockMode::AccessExclusive)?;
            super::validate_and_mark_constraint(engine, child, name)?;
        }
    }
    super::validate_and_mark_constraint(engine, table, name)?;
    Ok(true)
}

pub(super) fn merge_added_check(
    engine: &Engine,
    table: &str,
    mut incoming: TableCheck,
) -> Result<bool, SQLError> {
    let Some(name) = incoming.name.clone() else {
        return Ok(false);
    };
    let Some(mut existing) = find_check(engine, table, &name)? else {
        return Ok(false);
    };
    let (columns, constraints) = table_constraint_state(engine, table)?;
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    super::super::constraint_validation::validate_check_expression(
        engine,
        table,
        &relation.name,
        &columns,
        &mut incoming.expr,
    )?;
    crate::sql::reject_stored_regrole_constants(engine, &incoming.expr, None)?;
    if incoming.is_local && (existing.is_local || constraints.hierarchy.is_partition()) {
        return Err(super::super::check_inheritance::duplicate_check(
            &relation.name,
            &name,
        ));
    }
    super::super::check_inheritance::validate_check_merge(
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
        replace_check(engine, table, &name, existing)?;
    }
    engine.push_sql_notice(
        "NOTICE",
        &format!("merging constraint \"{name}\" with inherited definition"),
    );
    Ok(true)
}

pub(super) fn drop_check(
    engine: &Engine,
    table: &str,
    name: &str,
    recurse: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    let Some(check) = find_check(engine, table, name)? else {
        return Ok(false);
    };
    if !check.no_inherit && parent_count(engine, table, name)? > 0 {
        let relation =
            crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        return Err(constraint_error(
            "42P16",
            format!(
                "cannot drop inherited constraint \"{name}\" of relation \"{}\"",
                relation.name
            ),
        ));
    }
    drop_check_branch(engine, table, check, recurse, cascade, &mut BTreeSet::new())?;
    Ok(true)
}

fn drop_check_branch(
    engine: &Engine,
    table: &str,
    check: TableCheck,
    recurse: bool,
    cascade: bool,
    visiting: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    if !visiting.insert(table.to_string()) {
        return Err(SQLError::Internal(format!(
            "CHECK inheritance cycle reaches `{table}`"
        )));
    }
    let name = check
        .name
        .ok_or_else(|| SQLError::Internal("stored CHECK has no name".into()))?;
    let children = if check.no_inherit {
        Vec::new()
    } else {
        engine.direct_hierarchy_children(table)?
    };
    super::constraint_drop::drop_constraint_one(engine, table, &name, false, cascade)?;
    for child in children {
        engine.lock_relation(&child, crate::row_locks::RelationLockMode::AccessExclusive)?;
        engine.ensure_no_pending_trigger_events(&child, "ALTER TABLE")?;
        let mut child_check = find_check(engine, &child, &name)?.ok_or_else(|| {
            constraint_error(
                "42704",
                format!("constraint \"{name}\" of relation \"{child}\" does not exist"),
            )
        })?;
        let remaining = parent_count(engine, &child, &name)?;
        if recurse && !child_check.is_local && remaining == 0 {
            engine.ensure_table_owner(&child)?;
            drop_check_branch(engine, &child, child_check, true, cascade, visiting)?;
        } else if !recurse && remaining == 0 {
            child_check.is_local = true;
            replace_check(engine, &child, &name, child_check)?;
        }
    }
    visiting.remove(table);
    Ok(())
}

pub(super) fn rename_check(
    engine: &Engine,
    table: &str,
    from: &str,
    to: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    let Some(check) = find_check(engine, table, from)? else {
        return Ok(false);
    };
    if !check.no_inherit && !recurse && !engine.direct_hierarchy_children(table)?.is_empty() {
        return Err(constraint_error(
            "42P16",
            format!("inherited constraint \"{from}\" must be renamed in child tables too"),
        ));
    }
    let mut targets = if check.no_inherit || !recurse {
        vec![table.to_string()]
    } else {
        engine.hierarchy_scan_tables(table, true)?
    };
    // PostgreSQL checks descendants before the directly named constraint, so child ownership and name conflicts precede a root inheritance error.
    targets.retain(|target| target != table);
    targets.push(table.to_string());
    let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for target in targets {
        engine.lock_relation(&target, crate::row_locks::RelationLockMode::AccessExclusive)?;
        engine.ensure_table_owner(&target)?;
        let (columns, constraints) = table_constraint_state(engine, &target)?;
        let mut check = find_check(engine, &target, from)?.ok_or_else(|| {
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
        if !check.no_inherit && parent_count(engine, &target, from)? > expected {
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
        replace_check(engine, &target, from, check)?;
    }
    Ok(true)
}
