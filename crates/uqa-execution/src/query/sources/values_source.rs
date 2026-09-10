//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for VALUES sources.

/// Build the physical operator for a VALUES source.
use super::{
    eval_scalar, qualify_source_operator_with_columns, ColumnPrune, CteScope, PhysicalOperator,
    PlanSubqueryArena, SQLError, SQLParam, ScalarEvalContext, ScalarExpr, SourceContext,
    SourcePlan,
};

pub(super) fn build_values_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    prune: Option<&ColumnPrune>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
        } => {
            let inferred_types = crate::query::binding::values_types_in_scope(
                context.ctes.routines,
                rows,
                &ctes.scalar_subqueries,
                None,
                params,
                ctes,
            )?;
            let column_types = if internal_relation.is_some() {
                if !alias.is_none() || !column_aliases.is_empty() {
                    return Err(SQLError::Internal(
                        "internal VALUES carrier has SQL-visible aliases".into(),
                    ));
                }
                if !rows.is_empty()
                    && rows
                        .iter()
                        .any(|row| row.len() != internal_column_types.len())
                {
                    return Err(SQLError::Internal(
                        "internal VALUES carrier row width does not match its declared attributes"
                            .into(),
                    ));
                }
                internal_column_types.clone()
            } else {
                inferred_types
            };
            let source_columns = if column_aliases.is_empty() {
                (0..rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{}", index + 1))
                    .collect::<Vec<_>>()
            } else {
                column_aliases.clone()
            };
            let hook = context.relational.expression_scope(ctes.clone());
            let rows = build_values_physical_rows(
                context.types,
                hook.as_ref(),
                params,
                rows,
                &column_types,
            )?;
            if let Some(relation) = internal_relation {
                let schema =
                    crate::RowSchema::with_internal_relation_types(*relation, column_types);
                return Ok(Box::new(crate::TableScan::from_physical_rows(schema, rows)));
            }
            let schema = crate::RowSchema::with_types(source_columns.clone(), column_types);
            let operator: Box<dyn crate::PhysicalOperator + 'a> =
                Box::new(crate::TableScan::from_physical_rows(schema, rows));
            Ok(qualify_source_operator_with_columns(
                operator,
                &source_columns,
                alias.as_deref().unwrap_or_default(),
                prune,
                &[],
                ctes.lock_identities.emit,
            ))
        }
        _ => unreachable!("VALUES source builder called for a different source kind"),
    }
}

fn build_values_physical_rows(
    types: &dyn crate::FunctionTypeResolver,
    hook: &dyn crate::scalar::plan::QueryExpressionContext,
    params: &[SQLParam],
    rows: &[Vec<ScalarExpr>],
    column_types: &[Option<uqa_sql::ast::ColumnType>],
) -> Result<Vec<crate::PhysicalRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let subquery_arena = PlanSubqueryArena::new(hook.subquery_plans(), Some(hook));
    let ctx = ScalarEvalContext::new(None, params)
        .with_function_hook(hook)
        .with_subquery_runner(&subquery_arena);
    let empty_schema = crate::RowSchema::default();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for (i, expr) in row.iter().enumerate() {
            let source_type =
                crate::common_context_expression_type(expr, &empty_schema, params, Some(types))?;
            let v = uqa_sql::coerce_common_context_value(
                eval_scalar(expr, &ctx)?,
                source_type.as_ref(),
                column_types.get(i).and_then(Option::as_ref),
            )?;
            values.push(v);
        }
        out.push(crate::PhysicalRow::from_values(values));
    }
    Ok(out)
}
