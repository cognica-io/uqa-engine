//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical expression plans and query-backed subquery evaluation.

use uqa_core::Value;
use uqa_sql::expr::{EngineHook, RowLookup};
use uqa_sql::plan::{ExpressionPlan, QueryPlan};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::{
    eval_scalar, PhysicalRow, RowSchema, ScalarEvalContext, ScalarExpr, ScalarSubqueryRunner,
    SubqueryId, SubqueryResult,
};

/// Expression services needed by physical grouping and window operators.
/// Implementations retain statement scope without exposing catalog mutation or engine state.
pub trait QueryExpressionContext:
    EngineHook + PhysicalSubqueryRunner + crate::functions::AggregateFunctionRegistry
{
    fn expression_evaluator<'a>(
        &'a self,
        params: &'a [SQLParam],
    ) -> crate::SharedExpressionEvaluator<'a>;

    fn subquery_plans(&self) -> &[QueryPlan];
}

pub trait PhysicalSubqueryRunner {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError>;

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery(subquery, plan, outer_row, params)?
            .into_scalar_value()
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        plan: &QueryPlan,
        outer_row: PhysicalOuterRow<'_>,
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
        outer_row: PhysicalOuterRow<'_>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery(subquery, plan, outer_row, params)?
            .contains(needle)
    }
}

#[derive(Clone, Copy)]
pub enum PhysicalOuterRow<'a> {
    Absent,
    Physical {
        schema: &'a RowSchema,
        row: &'a PhysicalRow,
    },
}

impl PhysicalOuterRow<'_> {
    pub fn is_some(self) -> bool {
        matches!(self, Self::Physical { .. })
    }
}

pub struct PhysicalEvalContext<'a> {
    row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    row_schema: Option<&'a RowSchema>,
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn PhysicalSubqueryRunner>,
    physical_outer_row: Option<(&'a RowSchema, &'a PhysicalRow)>,
}

impl<'a> PhysicalEvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: None,
            row_lookup: Some(row),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    pub fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    pub fn with_row_schema(mut self, schema: &'a RowSchema) -> Self {
        self.row_schema = Some(schema);
        self
    }

    pub fn with_subquery_runner(mut self, runner: &'a dyn PhysicalSubqueryRunner) -> Self {
        self.subquery_runner = Some(runner);
        self
    }

    pub fn with_physical_outer_row(mut self, schema: &'a RowSchema, row: &'a PhysicalRow) -> Self {
        self.physical_outer_row = Some((schema, row));
        self
    }
}

pub fn eval_physical(
    expression: &ExpressionPlan,
    context: &PhysicalEvalContext<'_>,
) -> Result<Value, SQLError> {
    eval_physical_scalar(&expression.scalar, &expression.subqueries, context)
}

pub fn eval_physical_call_arguments(
    arguments: &[ExpressionPlan],
    context: &PhysicalEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    let (decoded, _) = analyze_physical_call_arguments(arguments)?;
    arguments
        .iter()
        .zip(decoded)
        .map(|(plan, argument)| {
            Ok((
                argument.name.map(str::to_string),
                eval_physical_scalar(argument.value, &plan.subqueries, context)?,
            ))
        })
        .collect()
}

pub use uqa_sql::ir::analyze_expression_call_arguments as analyze_physical_call_arguments;

pub fn eval_physical_scalar(
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
    if let Some(schema) = context.row_schema {
        scalar_context = scalar_context.with_row_schema(schema);
    }
    if context.subquery_runner.is_some() {
        scalar_context = scalar_context.with_subquery_runner(&subqueries);
    }
    if let Some((schema, row)) = context.physical_outer_row {
        scalar_context = scalar_context.with_physical_outer_row(schema, row);
    }
    eval_scalar(expression, &scalar_context)
}

pub struct PlanSubqueryArena<'a> {
    plans: &'a [QueryPlan],
    runner: Option<&'a dyn PhysicalSubqueryRunner>,
}

impl<'a> PlanSubqueryArena<'a> {
    pub fn new(plans: &'a [QueryPlan], runner: Option<&'a dyn PhysicalSubqueryRunner>) -> Self {
        Self { plans, runner }
    }
}

impl ScalarSubqueryRunner for PlanSubqueryArena<'_> {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        _outer_row: Option<&dyn RowLookup>,
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
            .execute_subquery(subquery, plan, PhysicalOuterRow::Absent, params)
    }

    fn execute_subquery_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?.execute_subquery(
            subquery,
            plan,
            PhysicalOuterRow::Physical {
                schema: outer_schema,
                row: outer_row,
            },
            params,
        )
    }

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        _outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .scalar_subquery_value(subquery, plan, PhysicalOuterRow::Absent, params)
    }

    fn scalar_subquery_value_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?.scalar_subquery_value(
            subquery,
            plan,
            PhysicalOuterRow::Physical {
                schema: outer_schema,
                row: outer_row,
            },
            params,
        )
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        _outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .subquery_exists(subquery, plan, PhysicalOuterRow::Absent, params)
    }

    fn subquery_exists_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?.subquery_exists(
            subquery,
            plan,
            PhysicalOuterRow::Physical {
                schema: outer_schema,
                row: outer_row,
            },
            params,
        )
    }

    fn subquery_contains(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        _outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?
            .subquery_contains(subquery, plan, needle, PhysicalOuterRow::Absent, params)
    }

    fn subquery_contains_physical(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        let plan = self.plan(subquery)?;
        self.runner()?.subquery_contains(
            subquery,
            plan,
            needle,
            PhysicalOuterRow::Physical {
                schema: outer_schema,
                row: outer_row,
            },
            params,
        )
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
    use uqa_sql::ast::{BinaryOp, Expr, FunctionBinding, FunctionDispatch};
    use uqa_sql::plan::ExpressionPlan;
    use uqa_sql::SQLParam;

    use super::{
        analyze_physical_call_arguments, eval_physical, eval_physical_call_arguments,
        PhysicalEvalContext,
    };

    #[test]
    fn physical_expression_plan_evaluates_the_scalar_field() {
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

    #[test]
    fn physical_call_arguments_preserve_and_unwrap_explicit_variadic_syntax() {
        let variadic_binding = FunctionBinding::dispatched(FunctionDispatch::VariadicArgument);
        let variadic = Expr::Func {
            name: variadic_binding.name.clone(),
            binding: Some(variadic_binding),
            args: vec![Expr::Literal(Value::Int(42))],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let named_binding = FunctionBinding::dispatched(FunctionDispatch::NamedArgument);
        let named = Expr::Func {
            name: named_binding.name.clone(),
            binding: Some(named_binding),
            args: vec![Expr::Literal(Value::Str("items".into())), variadic],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let arguments = vec![ExpressionPlan::lower(named)];

        let (decoded, explicit_variadic) = analyze_physical_call_arguments(&arguments).unwrap();
        assert!(explicit_variadic);
        assert_eq!(decoded[0].name, Some("items"));
        assert_eq!(
            eval_physical_call_arguments(&arguments, &PhysicalEvalContext::new(None, &[]),)
                .unwrap(),
            vec![(Some("items".into()), Value::Int(42))]
        );
    }
}
