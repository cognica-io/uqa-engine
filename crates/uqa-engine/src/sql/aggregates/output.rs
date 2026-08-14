//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection and HAVING evaluation for one finalized aggregate group.

use super::{
    aggregate_slot_index, aggregate_value_with_args, compile_having_aggregate_slots,
    compile_projection_aggregate_slots, eval_scalar, expr_references_columns, exprs_match,
    group_context_row, AggregateAccumulator, CteScope, Engine, PlanSubqueryArena, QueryBlockPlan,
    ResultRow, SQLError, SQLParam, ScalarEvalContext, ScalarExpr, ScopedEngineHook, SpillBuffer,
    Value,
};
use uqa_execution::{Batch, PhysicalRow, RowSchema};
use uqa_sql::expr::RowLookup;

pub(super) struct AggregateOutputPlan {
    finalizers: Vec<AggregateFinalizer>,
    projections: Vec<AggregateProjectionPlan>,
    having: Option<AggregateExpressionPlan>,
}

struct AggregateFinalizer {
    name: String,
    args: Vec<ScalarExpr>,
}

enum AggregateProjectionPlan {
    Evaluate(AggregateExpressionPlan),
    GroupValue(usize),
    Null,
    Invalid(usize),
}

struct AggregateExpressionPlan {
    expression: ScalarExpr,
    uses_lookup: bool,
    uses_group_row: bool,
}

impl AggregateOutputPlan {
    pub(super) fn compile(
        engine: &Engine,
        statement: &QueryBlockPlan,
        aggregate_targets: &[ScalarExpr],
        relaxed: bool,
    ) -> Result<Self, SQLError> {
        let finalizers = aggregate_targets
            .iter()
            .map(|target| match target {
                ScalarExpr::Func { name, args, .. } => Ok(AggregateFinalizer {
                    name: name.clone(),
                    args: args.clone(),
                }),
                _ => Err(SQLError::Internal(
                    "aggregate target is not a function".into(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut aggregate_cursor = 0;
        let projections = statement
            .projections
            .iter()
            .enumerate()
            .map(|(projection_index, projection)| {
                let first_aggregate = aggregate_cursor;
                let expression = compile_projection_aggregate_slots(
                    engine,
                    &projection.expr,
                    &mut aggregate_cursor,
                )?;
                if aggregate_cursor != first_aggregate {
                    let uses_group_row = references_external_row(&expression);
                    return Ok(AggregateProjectionPlan::Evaluate(AggregateExpressionPlan {
                        expression,
                        uses_lookup: true,
                        uses_group_row,
                    }));
                }
                if !expr_references_columns(&projection.expr) {
                    return Ok(AggregateProjectionPlan::Evaluate(AggregateExpressionPlan {
                        expression,
                        uses_lookup: false,
                        uses_group_row: false,
                    }));
                }
                if let Some(index) = statement
                    .group_by
                    .iter()
                    .position(|group| exprs_match(&projection.expr, group))
                {
                    return Ok(AggregateProjectionPlan::GroupValue(index));
                }
                if relaxed {
                    Ok(AggregateProjectionPlan::Null)
                } else {
                    Ok(AggregateProjectionPlan::Invalid(projection_index))
                }
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        if aggregate_cursor > finalizers.len() {
            return Err(SQLError::Internal(
                "aggregate projection slots exceed accumulator plan".into(),
            ));
        }

        let having = statement
            .having
            .as_ref()
            .map(|having| {
                let expression = compile_having_aggregate_slots(engine, having, aggregate_targets)?;
                let uses_group_row = references_external_row(&expression);
                Ok::<_, SQLError>(AggregateExpressionPlan {
                    expression,
                    uses_lookup: true,
                    uses_group_row,
                })
            })
            .transpose()?;

        Ok(Self {
            finalizers,
            projections,
            having,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finish_group(
    engine: &Engine,
    statement: &QueryBlockPlan,
    output_plan: &AggregateOutputPlan,
    accumulators: Vec<AggregateAccumulator>,
    group_values: &[Value],
    labels: &[String],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<Vec<Value>>, SQLError> {
    if accumulators.len() != output_plan.finalizers.len() {
        return Err(SQLError::Internal(format!(
            "aggregate accumulator count {} does not match output plan {}",
            accumulators.len(),
            output_plan.finalizers.len()
        )));
    }
    let aggregate_values = output_plan
        .finalizers
        .iter()
        .zip(&accumulators)
        .map(|(finalizer, accumulator)| {
            aggregate_value_with_args(&finalizer.name, accumulator, &finalizer.args)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let hook = ScopedEngineHook::new(engine, ctes);
    let subquery_arena = PlanSubqueryArena::new(&statement.subqueries, Some(&hook));
    debug_assert_eq!(labels.len(), statement.projections.len());
    let mut group_row = None;
    let mut values = Vec::with_capacity(output_plan.projections.len());

    for projection in &output_plan.projections {
        match projection {
            AggregateProjectionPlan::Evaluate(plan) if plan.uses_lookup => {
                let row = plan
                    .uses_group_row
                    .then(|| {
                        group_row.get_or_insert_with(|| group_context_row(statement, group_values))
                    })
                    .map(|row| &*row);
                let lookup = AggregateOutputLookup {
                    aggregate_values: &aggregate_values,
                    row,
                };
                let context = ScalarEvalContext::from_row_lookup(&lookup, params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena);
                values.push(eval_scalar(&plan.expression, &context)?);
            }
            AggregateProjectionPlan::Evaluate(plan) => {
                let context = ScalarEvalContext::new(None, params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena);
                values.push(eval_scalar(&plan.expression, &context)?);
            }
            AggregateProjectionPlan::GroupValue(index) => {
                values.push(group_values.get(*index).cloned().unwrap_or(Value::Null));
            }
            AggregateProjectionPlan::Null => values.push(Value::Null),
            AggregateProjectionPlan::Invalid(index) => {
                let label = labels.get(*index).map_or("?", String::as_str);
                return Err(SQLError::Unsupported(format!(
                    "non-aggregated projection `{label}` must appear in GROUP BY"
                )));
            }
        }
    }

    if let Some(having) = output_plan.having.as_ref() {
        let having_row = having.uses_group_row.then(|| {
            let mut row = group_row
                .take()
                .unwrap_or_else(|| group_context_row(statement, group_values));
            row.extend(labels.iter().cloned().zip(values.iter().cloned()));
            row
        });
        let lookup = AggregateOutputLookup {
            aggregate_values: &aggregate_values,
            row: having_row.as_ref(),
        };
        let context = ScalarEvalContext::from_row_lookup(&lookup, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&subquery_arena);
        if !uqa_sql::expr::truthy(&eval_scalar(&having.expression, &context)?) {
            return Ok(None);
        }
    }
    Ok(Some(values))
}

struct AggregateOutputLookup<'a> {
    aggregate_values: &'a [Value],
    row: Option<&'a ResultRow>,
}

impl RowLookup for AggregateOutputLookup<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        aggregate_slot_index(name)
            .and_then(|index| self.aggregate_values.get(index))
            .or_else(|| self.row.and_then(|row| row.column(name)))
    }

    fn qualified_column(&self, qualifier: &str, column: &str, key: &str) -> Option<&Value> {
        self.row
            .and_then(|row| row.qualified_column(qualifier, column, key))
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        if let Some(row) = self.row {
            row.visit_columns(visitor);
        }
    }

    fn visit_lookup_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        if let Some(row) = self.row {
            row.visit_lookup_columns(visitor);
        }
    }

    fn result_row(&self) -> Option<&ResultRow> {
        self.row
    }
}

fn references_external_row(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Star | ScalarExpr::QualifiedColumn { .. } => true,
        ScalarExpr::Column(column) => aggregate_slot_index(column).is_none(),
        ScalarExpr::Func { args, filter, .. } => {
            args.iter().any(references_external_row)
                || filter.as_deref().is_some_and(references_external_row)
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(references_external_row)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            references_external_row(lhs) || references_external_row(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. } => references_external_row(inner),
        ScalarExpr::IsNull { expr, .. } => references_external_row(expr),
        ScalarExpr::Between { expr, low, high } => {
            references_external_row(expr)
                || references_external_row(low)
                || references_external_row(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            references_external_row(expr) || list.iter().any(references_external_row)
        }
        ScalarExpr::WindowCall { args, .. } => args.iter().any(references_external_row),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(references_external_row)
                || when.iter().any(|(condition, result)| {
                    references_external_row(condition) || references_external_row(result)
                })
                || else_branch.as_deref().is_some_and(references_external_row)
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Default | ScalarExpr::Literal(_) | ScalarExpr::Param(_) => false,
    }
}

pub(super) fn push_output_row(
    output: &mut SpillBuffer,
    output_schema: &RowSchema,
    pending: &mut Vec<PhysicalRow>,
    values: Vec<Value>,
) -> Result<(), SQLError> {
    pending.push(PhysicalRow::from_values(values));
    if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
        output
            .push(Batch::from_physical_rows(
                output_schema.clone(),
                std::mem::take(pending),
            ))
            .map_err(super::sort_fallback::exec_to_sql_error)?;
        *pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

pub(super) fn flush_output_rows(
    output: &mut SpillBuffer,
    output_schema: &RowSchema,
    pending: &mut Vec<PhysicalRow>,
) -> Result<(), SQLError> {
    if !pending.is_empty() {
        output
            .push(Batch::from_physical_rows(
                output_schema.clone(),
                std::mem::take(pending),
            ))
            .map_err(super::sort_fallback::exec_to_sql_error)?;
    }
    Ok(())
}
