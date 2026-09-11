//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply routine removals while retaining the latest registry write guard through persistence.

use super::{
    analysis_dependencies, domain_dependencies, RoutineDropTarget, RoutineObjectDependents,
    RoutineRemovalContext, SQLError, SQLFunctionDropPlan,
};

pub fn commit_sql_function_drop(
    context: &RoutineRemovalContext<'_>,
    plan: SQLFunctionDropPlan,
) -> Result<(), SQLError> {
    let SQLFunctionDropPlan {
        domains,
        targets,
        dependents,
        notices,
    } = plan;
    let bindings = targets
        .iter()
        .map(RoutineDropTarget::binding)
        .collect::<Vec<_>>();
    let mut columns = domain_dependencies::domain_drop_column_names(&context.domains, &domains)?;
    columns.extend(
        dependents
            .columns
            .iter()
            .map(|(table, column, _)| (table.clone(), column.clone())),
    );
    let rewritten = context
        .bodies
        .prepare_routine_column_alias_drop(columns, &bindings)?;
    domain_dependencies::drop_domain_routine_checks(&context.domains, &bindings)?;
    drop_routine_object_dependents(context, &dependents)?;
    domain_dependencies::commit_domain_drop(&context.domains, &domains)?;
    commit_routine_registry_drop(context, &targets)?;
    context
        .bodies
        .publish_stored_routine_body_rewrites(rewritten)?;
    context.bodies.refresh_stored_merge_target_plans()?;
    for (level, message) in notices {
        context.notices.routine_drop_notice(level, &message);
    }
    Ok(())
}

pub fn drop_routine_check_dependent(
    context: &RoutineRemovalContext<'_>,
    table: &str,
    constraint: &str,
    foreign: bool,
) -> Result<(), SQLError> {
    if !foreign {
        return context
            .domains
            .tables
            .drop_constraint_dependency(table, constraint);
    }
    if context.foreign.drop_foreign_table_check_dependency(table, constraint)
        .map_err(|error| {
            SQLError::Internal(format!(
                "drop constraint `{constraint}` on foreign table `{table}` while cascading routine: {error}"
            ))
        })?
        == Some(true)
    {
        return Ok(());
    }
    Err(SQLError::Internal(format!(
        "constraint `{constraint}` on foreign table `{table}` disappeared after routine DROP preflight"
    )))
}

pub fn drop_routine_default_dependent(
    context: &RoutineRemovalContext<'_>,
    table: &str,
    column: &str,
    foreign: bool,
) -> Result<(), SQLError> {
    let dropped = if foreign {
        context
            .foreign
            .clear_foreign_table_default_dependency(table, column)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "drop default `{table}`.`{column}` while cascading routine: {error}"
                ))
            })?
            == Some(true)
    } else {
        context
            .tables
            .set_column_default_none(table, column)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "drop default `{table}`.`{column}` while cascading routine: {error}"
                ))
            })?
    };
    if dropped {
        return Ok(());
    }
    Err(SQLError::Internal(format!(
        "default `{table}`.`{column}` disappeared after routine DROP preflight"
    )))
}

pub fn drop_routine_generated_dependent(
    context: &RoutineRemovalContext<'_>,
    table: &str,
    column: &str,
    foreign: bool,
) -> Result<(), SQLError> {
    let dropped = if foreign {
        context
            .foreign
            .drop_foreign_table_generated_column_dependency(table, column)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "drop generated column `{table}`.`{column}` while cascading routine: {error}"
                ))
            })?
            == Some(true)
    } else {
        context
            .tables
            .try_drop_column_inner(table, column)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "drop generated column `{table}`.`{column}` while cascading routine: {error}"
                ))
            })?
    };
    if dropped {
        return Ok(());
    }
    Err(SQLError::Internal(format!(
        "generated column `{table}`.`{column}` disappeared after routine DROP preflight"
    )))
}

pub fn drop_routine_object_dependents(
    context: &RoutineRemovalContext<'_>,
    dependents: &RoutineObjectDependents,
) -> Result<(), SQLError> {
    for index in &dependents.indexes {
        context.domains.indexes.drop_index_dependency(index)?;
    }
    for (table, name) in &dependents.rules {
        context.events.drop_rule(&uqa_sql::ast::DropRule {
            name: name.clone(),
            table: table.clone(),
            if_exists: false,
            cascade: true,
        })?;
    }
    for (table, name) in &dependents.triggers {
        context.events.drop_trigger(&uqa_sql::ast::DropTrigger {
            name: name.clone(),
            table: table.clone(),
            if_exists: false,
            cascade: true,
        })?;
    }
    if !dependents.views.is_empty() {
        context
            .domains
            .views
            .drop_views_inner(&dependents.views, false)?;
    }
    for (table, constraint, foreign) in &dependents.checks {
        drop_routine_check_dependent(context, table, constraint, *foreign)?;
    }
    for (table, column, foreign) in &dependents.defaults {
        drop_routine_default_dependent(context, table, column, *foreign)?;
    }
    for (table, column, foreign) in &dependents.columns {
        drop_routine_generated_dependent(context, table, column, *foreign)?;
    }
    Ok(())
}

pub fn commit_routine_registry_drop(
    context: &RoutineRemovalContext<'_>,
    targets: &[RoutineDropTarget],
) -> Result<(), SQLError> {
    if targets.is_empty() {
        return Ok(());
    }
    let mut registry = context.registry.routines_write();
    let mut next = registry.clone();

    analysis_dependencies::remove_routine_registry_targets(&mut next, targets)?;
    context.publication.persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    context.changes.catalog_registry_changed();
    Ok(())
}
