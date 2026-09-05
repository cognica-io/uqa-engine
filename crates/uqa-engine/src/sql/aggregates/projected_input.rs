//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiled aggregate inputs for borrowed positional document rows.

use uqa_execution::{RowSchema, ScalarEvalContext, ScalarExpr};
use uqa_sql::expr::RowLookup;
use uqa_sql::SQLError;

use super::{
    is_json_array_aggregate, is_json_object_aggregate, is_ordered_set_aggregate,
    AggregateAccumulator, Engine,
};

mod integer;

use integer::{ProjectedIntegerExpression, ProjectedIntegerValue};

pub(super) struct ProjectedAggregatePlans {
    plans: Vec<ProjectedAggregatePlan>,
    all_direct: bool,
}

enum ProjectedAggregatePlan {
    Direct(ProjectedAggregateInput),
    General,
}

enum ProjectedAggregateInput {
    CountOne,
    Slot(usize),
    IntegerExpression {
        expression: ProjectedIntegerExpression,
        fallback: Box<ScalarExpr>,
    },
}

impl ProjectedAggregatePlans {
    pub(super) fn compile(
        engine: &Engine,
        aggregate_targets: &[ScalarExpr],
        input_schema: &RowSchema,
    ) -> Self {
        let plans = aggregate_targets
            .iter()
            .map(|expression| compile_plan(engine, expression, input_schema))
            .collect::<Vec<_>>();
        let all_direct = plans
            .iter()
            .all(|plan| matches!(plan, ProjectedAggregatePlan::Direct(_)));
        Self { plans, all_direct }
    }

    pub(super) fn all_direct(&self) -> bool {
        self.all_direct
    }

    pub(super) fn observe_direct<Row: RowLookup>(
        &self,
        accumulators: &mut [AggregateAccumulator],
        row: &Row,
        params: &[uqa_sql::SQLParam],
    ) -> Result<(), SQLError> {
        if !self.all_direct || self.plans.len() != accumulators.len() {
            return Err(SQLError::Internal(
                "direct projected aggregate plan lost target alignment".into(),
            ));
        }
        for (accumulator, plan) in accumulators.iter_mut().zip(&self.plans) {
            let ProjectedAggregatePlan::Direct(input) = plan else {
                unreachable!("all_direct excludes general aggregate plans");
            };
            match input {
                ProjectedAggregateInput::CountOne => {
                    accumulator.observe_projected_integer(1)?;
                }
                ProjectedAggregateInput::Slot(slot) => {
                    if let Some(value) = row.positional_column(*slot) {
                        accumulator.observe_projected(value)?;
                    }
                }
                ProjectedAggregateInput::IntegerExpression {
                    expression,
                    fallback,
                } => match expression.evaluate(row)? {
                    ProjectedIntegerValue::Integer(value) => {
                        accumulator.observe_projected_integer(value)?;
                    }
                    ProjectedIntegerValue::Null => {}
                    ProjectedIntegerValue::General => {
                        let context = ScalarEvalContext::from_row_lookup(row, params);
                        let value = uqa_execution::eval_scalar(fallback, &context)?;
                        accumulator.observe_projected(&value)?;
                    }
                },
            }
        }
        Ok(())
    }

    pub(super) fn observe<Row: RowLookup>(
        &self,
        accumulators: &mut [AggregateAccumulator],
        aggregate_targets: &[ScalarExpr],
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        if self.plans.len() != accumulators.len() || self.plans.len() != aggregate_targets.len() {
            return Err(SQLError::Internal(
                "projected aggregate plan lost target alignment".into(),
            ));
        }
        for (index, plan) in self.plans.iter().enumerate() {
            match plan {
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::CountOne) => {
                    accumulators[index].observe_projected_integer(1)?;
                }
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::Slot(slot)) => {
                    if let Some(value) = row.positional_column(*slot) {
                        accumulators[index].observe_projected(value)?;
                    }
                }
                ProjectedAggregatePlan::Direct(ProjectedAggregateInput::IntegerExpression {
                    expression,
                    fallback,
                }) => match expression.evaluate(row)? {
                    ProjectedIntegerValue::Integer(value) => {
                        accumulators[index].observe_projected_integer(value)?;
                    }
                    ProjectedIntegerValue::Null => {}
                    ProjectedIntegerValue::General => {
                        let value = uqa_execution::eval_scalar(fallback, context)?;
                        accumulators[index].observe_projected(&value)?;
                    }
                },
                ProjectedAggregatePlan::General => super::sort_fallback::observe_target(
                    &mut accumulators[index],
                    &aggregate_targets[index],
                    context,
                )?,
            }
        }
        Ok(())
    }
}

fn compile_plan(
    engine: &Engine,
    expression: &ScalarExpr,
    input_schema: &RowSchema,
) -> ProjectedAggregatePlan {
    let ScalarExpr::Func {
        name,
        args,
        distinct,
        order_by,
        filter,
        ..
    } = expression
    else {
        return ProjectedAggregatePlan::General;
    };
    if engine.registered_aggregate_function(name).is_some()
        || filter.is_some()
        || *distinct
        || !order_by.is_empty()
        || is_ordered_set_aggregate(name)
        || name.eq_ignore_ascii_case("string_agg")
        || is_json_object_aggregate(name)
        || is_json_array_aggregate(name)
    {
        return ProjectedAggregatePlan::General;
    }
    if name.eq_ignore_ascii_case("count")
        && (args.is_empty() || matches!(args.as_slice(), [ScalarExpr::Star]))
    {
        return ProjectedAggregatePlan::Direct(ProjectedAggregateInput::CountOne);
    }
    let Some(argument) = args.first() else {
        return ProjectedAggregatePlan::General;
    };
    if let Some(slot) = column_slot(argument, input_schema) {
        return ProjectedAggregatePlan::Direct(ProjectedAggregateInput::Slot(slot));
    }
    let Some(integer) = ProjectedIntegerExpression::compile(argument, input_schema) else {
        return ProjectedAggregatePlan::General;
    };
    ProjectedAggregatePlan::Direct(ProjectedAggregateInput::IntegerExpression {
        expression: integer,
        fallback: Box::new(argument.clone()),
    })
}

pub(super) fn column_slot(expression: &ScalarExpr, input_schema: &RowSchema) -> Option<usize> {
    match expression {
        ScalarExpr::Column(column) => input_schema.unqualified_position(column),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            input_schema.qualified_position(qualifier, column)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
