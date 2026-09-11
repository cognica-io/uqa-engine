//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collect schema, view, event, and index dependencies in catalog read order.

use super::{
    append_schema_function_dependents, ensure_no_function_dependencies, RoutineDropTarget,
    RoutineObjectDependents, RoutineRemovalContext, RoutineSchemaDependents, SQLError,
};

pub fn routine_object_dependents(
    context: &RoutineRemovalContext<'_>,
    targets: &[RoutineDropTarget],
    cascade: bool,
) -> Result<RoutineObjectDependents, SQLError> {
    let mut dependent_indexes = Vec::new();
    let mut dependent_views = Vec::new();
    let mut dependent_columns = Vec::new();
    let mut dependent_defaults = Vec::new();
    let mut dependent_checks = Vec::new();
    let mut dependent_triggers = Vec::new();
    let mut dependent_rules = Vec::new();
    for target in targets {
        if !target.is_procedure {
            let binding = target.binding();
            let dependents = direct_function_dependents(context, &binding)?;
            if cascade {
                dependent_indexes.extend(dependents.indexes);
                dependent_columns.extend(dependents.columns);
                dependent_defaults.extend(dependents.defaults);
                dependent_checks.extend(dependents.checks);
                dependent_views.extend(dependents.views);
                dependent_triggers.extend(dependents.triggers);
                dependent_rules.extend(dependents.rules);
            } else {
                ensure_no_function_dependencies(target, &dependents)?;
            }
        }
    }
    dependent_columns.sort();
    dependent_columns.dedup();
    for (table, column, _) in &dependent_columns {
        dependent_views.extend(
            context
                .domains
                .views
                .views_depending_on_column(table, column)
                .map_err(|error| {
                    SQLError::Internal(format!("inspect generated column views: {error}"))
                })?,
        );
    }
    dependent_defaults.sort();
    dependent_defaults.dedup();
    dependent_checks.sort();
    dependent_checks.dedup();
    dependent_views = context
        .domains
        .views
        .cascade_view_closure(dependent_views)?;
    if cascade && !dependent_views.is_empty() {
        dependent_rules.extend(
            context
                .dependencies
                .events
                .rules_depending_on_relations(&dependent_views)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "read rules depending on cascading function views: {error}"
                    ))
                })?
                .into_iter()
                .map(|(table, rule)| (table.qualified_name(), rule)),
        );
    }
    dependent_triggers.sort();
    dependent_triggers.dedup();
    dependent_rules.sort();
    dependent_rules.dedup();
    dependent_indexes.sort();
    dependent_indexes.dedup();
    Ok(RoutineObjectDependents {
        indexes: dependent_indexes,
        views: dependent_views,
        columns: dependent_columns,
        defaults: dependent_defaults,
        checks: dependent_checks,
        triggers: dependent_triggers,
        rules: dependent_rules,
    })
}

pub fn schema_function_dependents(
    context: &RoutineRemovalContext<'_>,
    target: &uqa_sql::ast::FunctionBinding,
) -> Result<RoutineSchemaDependents, SQLError> {
    let mut dependents = RoutineSchemaDependents::default();
    for (table_name, table) in context.dependencies.catalog.routine_table_schemas() {
        append_schema_function_dependents(
            &table_name,
            &table.columns(),
            &table.table_checks(),
            target,
            false,
            &mut dependents,
        )?;
    }
    for (relation, table) in context.dependencies.catalog.routine_foreign_tables().iter() {
        let table_name = relation.qualified_name();
        append_schema_function_dependents(
            &table_name,
            &table.columns,
            &table.checks,
            target,
            true,
            &mut dependents,
        )?;
    }
    dependents.columns.sort();
    dependents.columns.dedup();
    dependents.defaults.sort();
    dependents.defaults.dedup();
    dependents.checks.sort();
    dependents.checks.dedup();
    Ok(dependents)
}

fn direct_function_dependents(
    context: &RoutineRemovalContext<'_>,
    binding: &uqa_sql::ast::FunctionBinding,
) -> Result<RoutineObjectDependents, SQLError> {
    let schema = schema_function_dependents(context, binding)?;
    let views = context
        .dependencies
        .views
        .views_depending_on_function(binding)
        .map_err(|error| SQLError::Internal(format!("read view function dependencies: {error}")))?;
    let triggers = context
        .dependencies
        .events
        .triggers_depending_on_routine(binding)?;
    let rules = context
        .dependencies
        .events
        .rules_depending_on_routine(binding)
        .map_err(|error| SQLError::Internal(format!("read rule function dependencies: {error}")))?
        .into_iter()
        .map(|(table, rule)| (table.qualified_name(), rule))
        .collect::<Vec<_>>();
    Ok(RoutineObjectDependents {
        indexes: context
            .dependencies
            .indexes
            .indexes_depending_on_routine(binding)?,
        views,
        columns: schema.columns,
        defaults: schema.defaults,
        checks: schema.checks,
        triggers,
        rules,
    })
}
