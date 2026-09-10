//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RETURNING target schemas, alias visibility, projection expansion, and static analysis.
use super::returning_expression_schema;
use crate::{
    ast::{ColumnDef, ReturningAliases, Statement},
    binding::snapshot::BindingSnapshot,
    plan::{AggregateClassifier, ProjectionPlan},
    routines::RoutineResolution,
    ResultRow as Document, RowSchema, SQLError, SQLParam,
};
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};

pub trait ReturningCatalog {
    fn try_describe_table_row_type(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn try_table_columns(&self, table: &str) -> Result<Vec<String>, String>;
    fn view_schema(&self, table: &str) -> Result<Option<RowSchema>, SQLError>;
}
/// Capture names and types from an active scope without exposing execution state.
pub trait ReturningScope {
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError>;
}
#[derive(Clone, Copy)]
pub struct ReturningAnalysisContext<'a> {
    pub catalog: &'a dyn ReturningCatalog,
    pub routines: &'a dyn RoutineResolution,
    pub aggregates: &'a dyn AggregateClassifier,
    pub scope: &'a dyn ReturningScope,
}
fn dml_storage_error(action: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {error}"))
}

pub fn returning_target_schema(
    catalog: &dyn ReturningCatalog,
    table: &str,
) -> Result<RowSchema, SQLError> {
    let definitions = catalog
        .try_describe_table_row_type(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
    let Some(definitions) = definitions else {
        return catalog
            .view_schema(table)?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()));
    };
    if definitions.is_empty() {
        let columns = catalog
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
        let width = columns.len();
        return Ok(RowSchema::with_types(columns, vec![None; width]));
    }
    let columns = definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let types = definitions
        .into_iter()
        .map(|definition| Some(definition.ty))
        .collect();
    Ok(RowSchema::with_types(columns, types))
}

pub fn expanded_returning_projections(
    catalog: &dyn ReturningCatalog,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let columns = returning_target_schema(catalog, table)?.columns().to_vec();
    let mut projections = Vec::with_capacity(returning.len().max(columns.len()));
    for projection in returning {
        match &projection.expr {
            crate::ScalarExpr::Star => {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: crate::ScalarExpr::Column(column.clone()),
                    alias: Some(column.clone()),
                }));
            }
            crate::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: crate::ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    alias: Some(column.clone()),
                }));
            }
            _ => projections.push(projection.clone()),
        }
    }
    if !returning.is_empty() && projections.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "RETURNING must have at least one column".into(),
        });
    }
    Ok(projections)
}

pub fn dml_statement_returning_schema(
    context: ReturningAnalysisContext<'_>,
    statement: Statement,
) -> Result<Option<RowSchema>, SQLError> {
    let plan = crate::plan::UnifiedPlan::lower_with(statement, context.aggregates);
    let crate::plan::UnifiedPlan::Command(command) = plan else {
        return Ok(None);
    };
    dml_command_returning_schema(context, &command, &[])
}

pub fn dml_command_returning_schema(
    context: ReturningAnalysisContext<'_>,
    command: &crate::plan::CommandPlan,
    params: &[SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    match command {
        crate::plan::CommandPlan::Insert(plan) => analyze_dml_returning_plan(
            context,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            None,
            &plan.subqueries,
            params,
        ),
        crate::plan::CommandPlan::Update(plan) => analyze_dml_returning_plan(
            context,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
            params,
        ),
        crate::plan::CommandPlan::Delete(plan) => analyze_dml_returning_plan(
            context,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            &plan.ctes,
            plan.source.as_deref(),
            &plan.subqueries,
            params,
        ),
        _ => Ok(None),
    }
}

pub fn validate_insert_returning(
    context: ReturningAnalysisContext<'_>,
    plan: &crate::plan::InsertPlan,
    params: &[SQLParam],
    inherited: Option<&dyn ReturningScope>,
) -> Result<(), SQLError> {
    if plan.returning.is_empty() {
        return Ok(());
    }
    let target = returning_target_schema(context.catalog, &plan.table)?;
    if plan.source.is_none() {
        let width = if plan.columns.is_empty() {
            target.len()
        } else {
            plan.columns.len()
        };
        for row in &plan.rows {
            if row.len() > width || (!plan.columns.is_empty() && row.len() < width) {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: if row.len() > width {
                        "INSERT has more expressions than target columns"
                    } else {
                        "INSERT has more target columns than expressions"
                    }
                    .into(),
                });
            }
        }
    }
    let mut scope = inherited.unwrap_or(context.scope).binding_snapshot()?;
    for cte in &plan.ctes {
        scope.insert_deferred(cte.clone());
    }
    scope.scalar_subqueries.clone_from(&plan.subqueries);
    let expressions = returning_expression_schema(
        &target,
        &plan.target_qualifier,
        &plan.returning_aliases,
        None,
    );
    let projections = expanded_returning_projections(
        context.catalog,
        &plan.table,
        &plan.target_qualifier,
        &plan.returning_aliases,
        &plan.returning,
    )?;
    crate::binding::analyze_projection_output_schema(
        context.routines,
        &projections,
        &expressions,
        &target,
        &plan.subqueries,
        params,
        &scope.context(),
    )?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps RETURNING row types and nested scopes explicit"
)]
pub fn analyze_dml_returning_plan(
    context: ReturningAnalysisContext<'_>,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
    cte_plans: &[crate::plan::CtePlan],
    source: Option<&crate::plan::SourcePlan>,
    subqueries: &[crate::plan::QueryPlan],
    params: &[SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    if returning.is_empty() {
        return Ok(None);
    }
    let mut ctes = context.scope.binding_snapshot()?;
    for plan in cte_plans {
        ctes.insert_deferred(plan.clone());
    }
    ctes.scalar_subqueries = subqueries.to_vec();
    let supplemental = source
        .map(|source| {
            crate::binding::analyze_source_plan_schema(
                context.routines,
                source,
                params,
                &ctes.context(),
                None,
            )
        })
        .transpose()?;
    let star_schema = returning_target_schema(context.catalog, table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        target_qualifier,
        aliases,
        supplemental.as_ref(),
    );
    let projections = expanded_returning_projections(
        context.catalog,
        table,
        target_qualifier,
        aliases,
        returning,
    )?;
    crate::binding::analyze_projection_output_schema(
        context.routines,
        &projections,
        &expression_schema,
        &star_schema,
        subqueries,
        params,
        &ctes.context(),
    )
    .map(Some)
}

pub fn document_supplied_id(
    document: &Document,
    id_column: &str,
    auto_increment: bool,
) -> Result<Option<DocId>, SQLError> {
    match document.get(id_column) {
        Some(Value::Int(value)) if *value >= 0 => Ok(Some(*value as DocId)),
        Some(Value::Null) | None => Ok(None),
        Some(other) if auto_increment => Err(SQLError::TypeMismatch(format!(
            "auto-increment id must be an integer, got {other:?}"
        ))),
        Some(_) => Ok(None),
    }
}

pub fn validate_returning_alias_relations(
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut relation_names = BTreeSet::from([target_qualifier]);
    for (alias, explicit) in [
        (aliases.old.as_str(), aliases.old_explicit),
        (aliases.new.as_str(), aliases.new_explicit),
    ] {
        if !explicit {
            continue;
        }
        if relation_names.contains(alias)
            || supplemental.is_some_and(|schema| schema.has_qualifier(alias))
        {
            return Err(SQLError::Routine {
                sqlstate: "42712".into(),
                message: format!("table name \"{alias}\" specified more than once"),
            });
        }
        relation_names.insert(alias);
    }
    Ok(())
}
