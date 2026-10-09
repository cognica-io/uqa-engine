//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan a stored expression copy with the same SQL inlining and lazy simplification as executable statements.

use super::executable::StatementPlanningContext;
use crate::optimizer::{optimize_scalar_expression, OptimizerConfig};
use uqa_sql::{
    ast::{ColumnDef, Expr, FunctionVolatility},
    schema::{
        expressions::{analyze_schema_expression, PlannedSchemaExpression},
        SchemaBindingContext, SchemaExpressionCatalog,
    },
    SQLError,
};

/// Simplify a bound execution copy without resolving stored names again. Retain conditional type arms unless the caller has already supplied their coercions.
pub fn optimize_catalog_scalar(
    context: &StatementPlanningContext<'_>,
    expression: &mut uqa_sql::ScalarExpr,
) -> Result<(), SQLError> {
    optimize_scalar_expression(expression, &catalog_config(context))
}

fn catalog_config<'a>(context: &StatementPlanningContext<'a>) -> OptimizerConfig<'a> {
    let mut config = OptimizerConfig::new(context.constant_evaluator);
    config.builtin_permissions = context.optimization.builtin_permissions();
    config.routine_inlining = context.optimization.routine_inlining();
    config
}

/// Plan a rule's WHERE condition as a qualification. UNKNOWN and FALSE both reject a row here; they remain distinct in scalar/default/CHECK contexts.
pub fn optimize_rule_condition(
    context: &StatementPlanningContext<'_>,
    expression: &mut uqa_sql::plan::ExpressionPlan,
) -> Result<(), SQLError> {
    let config = catalog_config(context);
    optimize_scalar_expression(&mut expression.scalar, &config)?;
    if reject_unknown(&mut expression.scalar) {
        optimize_scalar_expression(&mut expression.scalar, &config)?;
    }
    Ok(())
}

fn reject_unknown(expression: &mut uqa_sql::ScalarExpr) -> bool {
    use uqa_core::Value;
    use uqa_sql::ScalarExpr;
    match expression {
        ScalarExpr::And(items) | ScalarExpr::Or(items) => items
            .iter_mut()
            .fold(false, |changed, item| reject_unknown(item) | changed),
        ScalarExpr::Literal(Value::Null)
        | ScalarExpr::TypedLiteral {
            value: Value::Null, ..
        } => {
            *expression = ScalarExpr::Literal(Value::Bool(false));
            true
        }
        // An intervening NOT, CASE, function or comparison observes SQL UNKNOWN; its children are value expressions.
        _ => false,
    }
}

pub fn plan_schema_expression(
    context: &StatementPlanningContext<'_>,
    catalog: &dyn SchemaExpressionCatalog,
    expression: &Expr,
    columns: &[ColumnDef],
) -> Result<PlannedSchemaExpression, SQLError> {
    let mut analyzed = None;
    context.analysis.scopes.with_scope(&mut |scope| {
        analyzed = Some(analyze_schema_expression(
            &SchemaBindingContext {
                catalog,
                binding: &scope.binding_context()?,
            },
            expression,
            columns,
        )?);
        Ok(())
    })?;
    let mut analyzed = analyzed
        .ok_or_else(|| SQLError::Internal("schema expression analysis did not run".into()))?;
    let routines = context.optimization.routine_inlining().ok_or_else(|| {
        SQLError::Internal("schema expression planning requires routine metadata".into())
    })?;
    let mut config = OptimizerConfig::new(context.constant_evaluator);
    config.builtin_permissions = context.optimization.builtin_permissions();
    config.routine_inlining = Some(routines);
    config.coerced_conditionals = true;
    optimize_scalar_expression(&mut analyzed.scalar, &config)?;
    let immutable = routines.expression_volatility(&analyzed.scalar, &analyzed.schema)?
        == FunctionVolatility::Immutable;
    let constant = matches!(
        analyzed.scalar,
        uqa_sql::ScalarExpr::Literal(_) | uqa_sql::ScalarExpr::TypedLiteral { .. }
    );
    Ok(PlannedSchemaExpression {
        expression: analyzed.expression,
        ty: analyzed.ty,
        immutable,
        constant,
    })
}

#[cfg(test)]
mod tests {
    use super::reject_unknown;
    use uqa_core::Value;
    use uqa_sql::ScalarExpr;

    #[test]
    fn qualification_rewrite_preserves_all_three_valued_acceptance_combinations() {
        for a in [Value::Null, Value::Bool(false), Value::Bool(true)] {
            for b in [Value::Null, Value::Bool(false), Value::Bool(true)] {
                for nested in [
                    ScalarExpr::And(vec![
                        ScalarExpr::Literal(a.clone()),
                        ScalarExpr::Literal(b.clone()),
                    ]),
                    ScalarExpr::Or(vec![
                        ScalarExpr::Literal(a.clone()),
                        ScalarExpr::Literal(b.clone()),
                    ]),
                ] {
                    for before in [
                        nested.clone(),
                        ScalarExpr::Not(Box::new(nested.clone())),
                        ScalarExpr::IsNull {
                            expr: Box::new(nested),
                            negated: false,
                        },
                    ] {
                        let mut after = before.clone();
                        reject_unknown(&mut after);
                        let accepts = |expression: &ScalarExpr| {
                            matches!(
                                uqa_execution::scalar::eval_constant_scalar(expression).unwrap(),
                                Value::Bool(true)
                            )
                        };
                        assert_eq!(accepts(&before), accepts(&after), "{before:?}");
                    }
                }
            }
        }
    }
}
