//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical expansion of set-returning SELECT-list calls.

use crate::query::routine_invocation::SQLRoutineInvoker;
use crate::query::table_functions::{
    registered_table_function_rows, TableFunctionCall, TableFunctionRows,
};
use crate::query::PhysicalProjection;
use crate::scalar::plan::PlanSubqueryArena;
use crate::{
    eval_call_arguments, Batch, ExecResult, OwnedPhysicalRow, PhysicalOperator,
    PhysicalProjectRows, PhysicalRow, Project, ProjectionTarget, RowSchema, ScalarEvalContext,
    ScalarExpr, SharedExpressionEvaluator,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::semantics::sets::{SetFunctionCall, SetFunctionCatalog, SetProjectionPlan};
use uqa_sql::{ast::ColumnType, SQLError, SQLParam};

/// Function invocation and scoped expression services consumed by a set-expansion operator.
pub trait SetFunctionRuntime:
    crate::query::expression::ScalarExpressionContext + SQLRoutineInvoker
{
    fn has_registered_table_function(&self, name: &str) -> bool;
    fn table_function_rows(
        &self,
        call: TableFunctionCall<'_>,
        params: &[SQLParam],
        row: Option<&OwnedPhysicalRow>,
    ) -> Result<TableFunctionRows, SQLError>;
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

/// Projection and output batching applied after set expansion.
pub struct SetProjectionOutput {
    pub projections: Vec<PhysicalProjection>,
    pub pass_through: bool,
    pub batch_size: usize,
}

pub fn build_set_projection<'a>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    catalog: &dyn SetFunctionCatalog,
    runtime: Arc<dyn SetFunctionRuntime + 'a>,
    params: &'a [SQLParam],
    evaluator: SharedExpressionEvaluator<'a>,
    output: SetProjectionOutput,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SetProjectionOutput {
        projections,
        pass_through,
        batch_size: output_batch_size,
    } = output;
    let plan = SetProjectionPlan::new(
        catalog,
        runtime.as_ref(),
        projections,
        operator.row_schema(),
        params,
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
            Arc::clone(&runtime),
            params,
            evaluator.clone(),
            SetProjectionPlan { projections, calls },
            true,
            output_batch_size,
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
pub use operator::SetProjection;
