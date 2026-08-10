//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine adapter for the shared scalar physical evaluator.

use uqa_core::Value;
use uqa_execution::{
    eval_scalar, ScalarEvalContext, ScalarExpr, ScalarSubqueryRunner, SubqueryId, SubqueryResult,
};
use uqa_planner::{ExpressionPlan, QueryPlan};
use uqa_sql::expr::{EngineHook, RowLookup, NAMED_ARG_FUNCTION};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use super::{CteScope, Engine, ScopedEngineHook};

pub(super) trait PhysicalSubqueryRunner {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError>;

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery(subquery, plan, outer_row, params)?
            .into_scalar_value()
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.execute_subquery(subquery, plan, outer_row, params)?
            .into_exists()
    }

    fn subquery_contains(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        needle: &Value,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery(subquery, plan, outer_row, params)?
            .contains(needle)
    }
}

pub(super) struct PhysicalEvalContext<'a> {
    row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn PhysicalSubqueryRunner>,
}

impl<'a> PhysicalEvalContext<'a> {
    pub(super) fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            params,
            function_hook: None,
            subquery_runner: None,
        }
    }

    pub(super) fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: None,
            row_lookup: Some(row),
            params,
            function_hook: None,
            subquery_runner: None,
        }
    }

    pub(super) fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    pub(super) fn with_subquery_runner(mut self, runner: &'a dyn PhysicalSubqueryRunner) -> Self {
        self.subquery_runner = Some(runner);
        self
    }
}

pub(super) fn eval_physical(
    expression: &ExpressionPlan,
    context: &PhysicalEvalContext<'_>,
) -> Result<Value, SQLError> {
    eval_physical_scalar(&expression.scalar, &expression.subqueries, context)
}

/// Lower an AST expression that belongs to a schema or procedural boundary,
/// then execute the resulting physical scalar IR. Runtime consumers never
/// invoke the AST evaluator or dispatch an AST subquery directly.
pub(super) fn eval_lowered_expression(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let expression = ExpressionPlan::lower(expression.clone());
    let scope = CteScope::new();
    let hook = ScopedEngineHook::new(engine, &scope);
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    eval_physical(&expression, &context)
}

pub(super) fn eval_physical_call_arguments(
    arguments: &[ExpressionPlan],
    context: &PhysicalEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    arguments
        .iter()
        .map(|argument| match &argument.scalar {
            ScalarExpr::Func {
                name,
                args: marker_args,
                ..
            } if name == NAMED_ARG_FUNCTION => {
                let Some(ScalarExpr::Literal(Value::Str(argument_name))) = marker_args.first()
                else {
                    return Err(SQLError::Internal("named argument without a name".into()));
                };
                let value = marker_args
                    .get(1)
                    .ok_or_else(|| SQLError::Internal("named argument without a value".into()))?;
                Ok((
                    Some(argument_name.to_ascii_lowercase()),
                    eval_physical_scalar(value, &argument.subqueries, context)?,
                ))
            }
            _ => Ok((None, eval_physical(argument, context)?)),
        })
        .collect()
}

pub(super) fn eval_physical_scalar(
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    context: &PhysicalEvalContext<'_>,
) -> Result<Value, SQLError> {
    let subqueries = PlanSubqueryArena::new(subqueries, context.subquery_runner);
    let mut scalar_context = context.row_lookup.map_or_else(
        || ScalarEvalContext::new(context.row, context.params),
        |row| ScalarEvalContext::from_row_lookup(row, context.params),
    );
    if let Some(hook) = context.function_hook {
        scalar_context = scalar_context.with_function_hook(hook);
    }
    if context.subquery_runner.is_some() {
        scalar_context = scalar_context.with_subquery_runner(&subqueries);
    }
    eval_scalar(expression, &scalar_context)
}

pub(super) struct PlanSubqueryArena<'a> {
    plans: &'a [QueryPlan],
    runner: Option<&'a dyn PhysicalSubqueryRunner>,
}

impl<'a> PlanSubqueryArena<'a> {
    pub(super) fn new(
        plans: &'a [QueryPlan],
        runner: Option<&'a dyn PhysicalSubqueryRunner>,
    ) -> Self {
        Self { plans, runner }
    }
}

impl ScalarSubqueryRunner for PlanSubqueryArena<'_> {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        let plan = self.plans.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "physical scalar subquery slot {subquery} is out of bounds"
            ))
        })?;
        self.runner
            .ok_or_else(|| {
                SQLError::Unsupported("physical subquery requires a plan runner".into())
            })?
            .execute_subquery(subquery, plan, outer_row, params)
    }

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .scalar_subquery_value(subquery, plan, outer_row, params)
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .subquery_exists(subquery, plan, outer_row, params)
    }

    fn subquery_contains(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .subquery_contains(subquery, plan, needle, outer_row, params)
    }
}

impl PlanSubqueryArena<'_> {
    fn plan(&self, subquery: SubqueryId) -> Result<&QueryPlan, SQLError> {
        self.plans.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "physical scalar subquery slot {subquery} is out of bounds"
            ))
        })
    }

    fn runner(&self) -> Result<&dyn PhysicalSubqueryRunner, SQLError> {
        self.runner
            .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))
    }
}

#[cfg(test)]
mod tests {
    use uqa_core::Value;
    use uqa_planner::ExpressionPlan;
    use uqa_sql::ast::{BinaryOp, Expr};
    use uqa_sql::SQLParam;

    use super::{eval_physical, PhysicalEvalContext};

    #[test]
    fn engine_evaluates_the_physical_expression_field() {
        let expression = ExpressionPlan::lower(Expr::Binary {
            op: BinaryOp::Multiply,
            lhs: Box::new(Expr::Param(1)),
            rhs: Box::new(Expr::Literal(Value::Int(3))),
        });
        let params = [SQLParam::Scalar(Value::Int(7))];
        assert_eq!(
            eval_physical(&expression, &PhysicalEvalContext::new(None, &params)).unwrap(),
            Value::Int(21)
        );
    }
}
