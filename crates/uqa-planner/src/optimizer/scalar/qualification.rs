//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Simplify the top-level AND/OR tree where only TRUE admits a row.

use super::{
    constants::literal_value_with_control, optimize_scalar_slot, OptimizerConfig, SQLError,
    ScalarExpr, Value,
};

pub(crate) fn optimize_qualification(
    expression: &mut ScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    // Fold every reachable constant first, retaining planning-error order before the qualification can discard a nullable branch.
    optimize_scalar_slot(expression, config)?;
    if config.enable_boolean_simplify {
        simplify(expression, &config.production)?;
    }
    Ok(())
}

fn simplify(
    expression: &mut ScalarExpr,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    let and = matches!(expression, ScalarExpr::And(_));
    let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = expression else {
        // A projection, NOT, Boolean test or function argument observes three-valued truth. Do not move the qualification boundary through it.
        return Ok(());
    };
    for item in items.iter_mut() {
        simplify(item, control)?;
        let constant = match literal_value_with_control(item, control)?.as_deref() {
            Some(Value::Bool(value)) => Some(*value),
            Some(Value::Null) => Some(false),
            _ => None,
        };
        if constant == Some(!and) {
            *expression = ScalarExpr::Literal(Value::Bool(!and));
            return Ok(());
        }
        if constant == Some(and) {
            *item = ScalarExpr::Literal(Value::Bool(and));
        }
    }
    items.retain(|item| !matches!(item, ScalarExpr::Literal(Value::Bool(value)) if *value == and));
    match items.len() {
        0 => *expression = ScalarExpr::Literal(Value::Bool(and)),
        1 => *expression = items.pop().expect("one remaining qualification"),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::constants::literal_value;
    use super::*;

    fn expression(sql: &str) -> ScalarExpr {
        let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(sql).unwrap().remove(0) else {
            panic!("SELECT expression");
        };
        uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar
    }

    #[test]
    fn strict_null_qualifications_discard_volatile_comparisons() {
        let config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        for sql in [
            "SELECT nextval('ticks') BETWEEN SYMMETRIC NULL::bigint AND 100",
            "SELECT nextval('ticks') > 0 AND NULL::boolean",
            "SELECT (nextval('ticks') > 0 AND NULL::boolean) OR false",
        ] {
            let mut value = expression(sql);
            optimize_qualification(&mut value, &config).unwrap();
            assert_eq!(value, ScalarExpr::Literal(Value::Bool(false)), "{sql}");
        }
    }

    #[test]
    fn nullable_scalar_and_boolean_tests_keep_their_observations() {
        let config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        let mut scalar = expression("SELECT nextval('ticks') > 0 AND NULL::boolean");
        optimize_scalar_slot(&mut scalar, &config).unwrap();
        assert!(matches!(scalar, ScalarExpr::And(_)));
        for sql in [
            "SELECT (nextval('ticks') > 0 AND NULL::boolean) IS NOT FALSE",
            "SELECT NOT (nextval('ticks') > 0 AND NULL::boolean)",
            "SELECT coalesce(nextval('ticks') > 0 AND NULL::boolean, true)",
        ] {
            let mut value = expression(sql);
            optimize_qualification(&mut value, &config).unwrap();
            assert!(literal_value(&value).unwrap().is_none(), "{sql}");
        }
    }

    #[test]
    fn null_qualifications_preserve_constant_error_order() {
        let config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        let mut value = expression("SELECT NULL::boolean AND (1 / 0 > 0)");
        assert_eq!(
            optimize_qualification(&mut value, &config)
                .unwrap_err()
                .sqlstate(),
            Some("22012")
        );
    }
}
