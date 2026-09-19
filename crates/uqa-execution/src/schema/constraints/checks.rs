//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute recursive CHECK merging.
use super::{
    ddl_storage_error, find_constraint, publish_constraint_state, table_constraint_state,
    validate_check_expression, ConstraintAlterContext, ConstraintLocation, SQLError,
};
use uqa_sql::ast::TableCheck;

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
