//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column closure and routine removals triggered by a column deletion.

use super::{
    commit_sql_function_drop, expand_routine_domain_column_drop, routine_object_dependents,
    BTreeMap, BTreeSet, RoutineDropResolution, RoutineRemovalContext, SQLError,
    SQLFunctionDropPlan,
};

pub fn drop_column_routine_dependents(
    context: &RoutineRemovalContext<'_>,
    table: &str,
    column: &str,
    cascade: bool,
) -> Result<(), SQLError> {
    let registry = context.registry.routine_snapshot();
    if registry.is_empty() {
        return Ok(());
    }
    let mut resolution = RoutineDropResolution {
        targets: Vec::new(),
        seen_targets: BTreeSet::new(),
        notices: Vec::new(),
    };
    let mut domains = BTreeSet::new();
    expand_routine_domain_column_drop(
        context,
        &registry,
        &mut resolution,
        &mut domains,
        BTreeSet::from([(table.to_string(), column.to_string())]),
    )?;
    if resolution.targets.is_empty() {
        return Ok(());
    }
    if !cascade {
        let oid = crate::catalog::projection::resolve_bound_regclass_oid(&context.catalog, table)?
            .ok_or_else(|| SQLError::Internal("DROP COLUMN table disappeared".into()))?;
        let label = crate::catalog::projection::resolve_regtype_output(
            &context.catalog,
            &uqa_sql::ast::ColumnType::Regclass,
            oid,
        )
        .map_err(SQLError::Internal)?
        .unwrap_or_else(|| table.to_string());
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop column {column} of table {label} because other objects depend on it"
            ),
        });
    }
    let dependents = routine_object_dependents(context, &resolution.targets, true)?;
    commit_sql_function_drop(
        context,
        SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        },
    )
}

pub fn expand_column_drop_dependencies(
    context: &RoutineRemovalContext<'_>,
    columns: &mut BTreeSet<(String, String)>,
    relations: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    if columns.is_empty() {
        return Ok(());
    }
    let mut tables = context
        .dependencies
        .catalog
        .routine_table_schemas()
        .into_iter()
        .map(|(name, table)| (name, (table.object_id(), table.columns().clone())))
        .collect::<BTreeMap<_, _>>();
    tables.extend(
        context
            .dependencies
            .catalog
            .routine_foreign_tables()
            .iter()
            .map(|(identity, table)| {
                (
                    identity.qualified_name(),
                    (table.object_id, table.columns.clone()),
                )
            }),
    );
    let mut pending = columns.iter().cloned().collect::<Vec<_>>();
    while let Some((table, column)) = pending.pop() {
        let (table_id, definitions) = tables
            .get(&table)
            .ok_or_else(|| SQLError::Internal(format!("dependent table {table} disappeared")))?;
        let column_id = definitions
            .iter()
            .find(|definition| definition.name == column)
            .and_then(|definition| definition.object_id)
            .ok_or_else(|| {
                SQLError::Internal(format!("dependent column {table}.{column} has no identity"))
            })?;
        relations.extend(
            context
                .dependencies
                .sequences
                .sequence_names_owned_by_column(*table_id, column_id)
                .map_err(|error| {
                    SQLError::Internal(format!("inspect column sequences: {error}"))
                })?,
        );
        relations.extend(
            context
                .domains
                .views
                .views_depending_on_column(&table, &column)
                .map_err(|error| SQLError::Internal(format!("inspect column views: {error}")))?,
        );
        for definition in definitions {
            if definition.generated.as_ref().is_some_and(|generated| {
                uqa_sql::schema::dependencies::schema_expr_references_column(
                    &generated.expression,
                    &column,
                )
            }) {
                let dependent = (table.clone(), definition.name.clone());
                if columns.insert(dependent.clone()) {
                    pending.push(dependent);
                }
            }
        }
    }
    Ok(())
}
