//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-style set-returning SELECT-list projection.

use uqa_core::Value;
use uqa_execution::{
    eval_call_arguments, Batch, ExecResult, OwnedPhysicalRow, PhysicalOperator,
    PhysicalProjectRows, PhysicalRow, Project, ProjectionTarget, RowSchema, ScalarEvalContext,
    ScalarExpr, SharedExpressionEvaluator,
};
use uqa_planner::QueryBlockPlan;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

use super::{CteScope, Engine, PhysicalProjection, ScopedEngineHook};
use crate::sql::scalar::PlanSubqueryArena;

#[derive(Clone)]
struct SetFunctionCall {
    placeholder: uqa_sql::ast::InternalColumnRef,
    name: String,
    binding: Option<FunctionBinding>,
    args: Vec<ScalarExpr>,
    level: usize,
}

struct SetProjectionPlan {
    projections: Vec<PhysicalProjection>,
    calls: Vec<SetFunctionCall>,
    output_batch_size: usize,
}

pub(in crate::sql) struct AggregateOutputProjectionPlan {
    pub(in crate::sql) statement: QueryBlockPlan,
    pub(in crate::sql) projections: Vec<PhysicalProjection>,
}

pub(in crate::sql) struct GroupSetProjectionPlan {
    pub(in crate::sql) statement: QueryBlockPlan,
    pub(in crate::sql) projections: Vec<PhysicalProjection>,
}

enum SetFunctionState {
    Scalar(Value),
    Set {
        columns: Vec<String>,
        rows: PhysicalProjectRows,
        exhausted: bool,
    },
}

struct SetExpansion {
    input: OwnedPhysicalRow,
    calls: Vec<SetFunctionState>,
    has_set: bool,
    scalar_emitted: bool,
}

impl SetExpansion {
    fn next_values(&mut self) -> ExecResult<Option<Vec<Value>>> {
        if !self.has_set {
            if self.scalar_emitted {
                return Ok(None);
            }
            self.scalar_emitted = true;
            return Ok(Some(
                self.calls
                    .iter()
                    .map(|call| match call {
                        SetFunctionState::Scalar(value) => value.clone(),
                        SetFunctionState::Set { .. } => unreachable!("has_set is false"),
                    })
                    .collect(),
            ));
        }

        let mut produced = false;
        let mut values = Vec::with_capacity(self.calls.len());
        for call in &mut self.calls {
            match call {
                SetFunctionState::Scalar(value) => values.push(value.clone()),
                SetFunctionState::Set {
                    columns,
                    rows,
                    exhausted,
                } => {
                    if *exhausted {
                        values.push(Value::Null);
                        continue;
                    }
                    if let Some(row) = rows.next() {
                        produced = true;
                        values.push(set_row_value(row?, columns));
                    } else {
                        *exhausted = true;
                        values.push(Value::Null);
                    }
                }
            }
        }
        Ok(produced.then_some(values))
    }
}

fn set_row_value(row: PhysicalRow, columns: &[String]) -> Value {
    let values = row.into_physical_values();
    if values.len() == 1 {
        return values.into_iter().next().unwrap_or(Value::Null);
    }
    debug_assert_eq!(columns.len(), values.len());
    Value::Record(columns.iter().cloned().zip(values).collect())
}

mod rewrite;
mod validation;

use rewrite::rewrite_set_calls;
pub(in crate::sql) use rewrite::{
    prepare_aggregate_output_projection, prepare_group_set_projection,
};
pub(in crate::sql) use validation::{
    builtin_returns_set, expression_may_return_set, projections_may_return_set,
    validate_query_set_contexts, validate_source_set_contexts_before_build,
    validate_values_set_contexts,
};

impl SetProjectionPlan {
    fn new(
        engine: &Engine,
        resolver: &dyn uqa_execution::FunctionTypeResolver,
        projections: Vec<PhysicalProjection>,
        schema: &RowSchema,
        params: &[SQLParam],
        output_batch_size: usize,
    ) -> Result<Self, SQLError> {
        let mut calls = Vec::new();
        let call_relation = uqa_sql::ast::InternalRelationId::allocate();
        let projections = projections
            .into_iter()
            .map(|(target, expression)| {
                Ok((
                    target,
                    rewrite_set_calls(
                        engine,
                        resolver,
                        expression,
                        &mut calls,
                        call_relation,
                        schema,
                        params,
                    )?,
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        debug_assert!(!calls.is_empty());
        Ok(Self {
            projections,
            calls,
            output_batch_size,
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
pub(in crate::sql) fn build_set_projection<'a>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    projections: Vec<PhysicalProjection>,
    pass_through: bool,
    output_batch_size: usize,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let resolver = ScopedEngineHook::new(engine, ctes);
    let plan = SetProjectionPlan::new(
        engine,
        &resolver,
        projections,
        operator.row_schema(),
        params,
        output_batch_size,
    )?;
    let max_level = plan.calls.iter().map(|call| call.level).max().unwrap_or(0);
    for level in 0..=max_level {
        let calls = plan
            .calls
            .iter()
            .filter(|call| call.level == level)
            .cloned()
            .collect::<Vec<_>>();
        if calls.is_empty() {
            continue;
        }
        let projections = calls
            .iter()
            .map(|call| {
                (
                    ProjectionTarget::Internal(call.placeholder),
                    ScalarExpr::InternalColumn(call.placeholder),
                )
            })
            .collect();
        operator = Box::new(SetProjection::from_plan(
            operator,
            engine,
            params,
            ctes,
            evaluator.clone(),
            SetProjectionPlan {
                projections,
                calls,
                output_batch_size: plan.output_batch_size,
            },
            true,
        ));
    }
    if pass_through {
        Ok(Box::new(Project::appending_target_evaluator(
            operator,
            plan.projections,
            evaluator,
        )))
    } else {
        Ok(Box::new(Project::with_target_evaluator(
            operator,
            plan.projections,
            evaluator,
        )))
    }
}

mod operator;
pub(in crate::sql) use operator::SetProjection;
