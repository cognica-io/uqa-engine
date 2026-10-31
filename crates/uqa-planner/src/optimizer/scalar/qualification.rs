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
use uqa_core::memory::ProductionControl;
use uqa_sql::plan::OwnedScalarExpr;

pub(crate) fn optimize_qualification_owned(
    expression: OwnedScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let expression = super::owned::optimize(expression, config)?;
    if config.enable_boolean_simplify {
        simplify_owned(expression, &config.production, &config.production)
    } else {
        Ok(expression)
    }
}

pub(crate) fn optimize_qualification(
    expression: &mut ScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    // Fold every reachable constant first, retaining planning-error order before the qualification can discard a nullable branch.
    optimize_scalar_slot(expression, config)?;
    if config.enable_boolean_simplify {
        let input = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
        let storage = ProductionControl::uncontrolled();
        *expression = simplify_owned(
            OwnedScalarExpr::uncontrolled(input),
            &storage,
            &config.production,
        )?
        .finish(&storage)?
        .into_uncontrolled()
        .expect("ordinary qualification owner");
    }
    Ok(())
}

pub(in crate::optimizer) fn simplify_owned(
    mut expression: OwnedScalarExpr,
    storage: &ProductionControl<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let and = matches!(&*expression, ScalarExpr::And(_));
    if !matches!(&*expression, ScalarExpr::And(_) | ScalarExpr::Or(_)) {
        // A projection, NOT, Boolean test or function argument observes three-valued truth. Do not move the qualification boundary through it.
        return Ok(expression);
    }
    let mut kept = 0;
    for index in 0..expression.child_count() {
        let mut constant = None;
        expression = expression.map_child(index, storage, |item| {
            let item = simplify_owned(item, storage, control)?;
            constant = match literal_value_with_control(&item, control)?.as_deref() {
                Some(Value::Bool(value)) => Some(*value),
                Some(Value::Null) => Some(false),
                _ => None,
            };
            if constant == Some(and) {
                drop(item);
                inline_boolean(and, storage)
            } else {
                Ok(item)
            }
        })?;
        if constant == Some(!and) {
            drop(expression);
            return inline_boolean(!and, storage);
        }
        if constant != Some(and) {
            expression = expression.swap_children(index, kept, storage)?;
            kept += 1;
        }
    }
    expression = expression.truncate_boolean_children(kept, storage)?;
    match kept {
        0 => {
            drop(expression);
            inline_boolean(and, storage)
        }
        1 => Ok(expression.select_child(0)),
        _ => Ok(expression),
    }
}

fn inline_boolean(
    value: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::literal(
        control.finish(Value::Bool(value), control.empty_reservation())?,
        control,
    )?)
}

#[cfg(test)]
mod tests {
    use super::super::constants::literal_value;
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn admitted_qualification_retires_only_discarded_children_without_new_storage() {
        let survivor = "retained".repeat(1024);
        let source = ScalarExpr::Or(vec![
            ScalarExpr::And(vec![
                ScalarExpr::Column("discarded".repeat(4096).into()),
                ScalarExpr::Literal(Value::Null),
            ]),
            ScalarExpr::Column(survivor.clone().into()),
            ScalarExpr::Literal(Value::Bool(false)),
        ]);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        let before = budget.used();
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(simplify_owned(input, &control, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(allocation.count_total, 0);
        assert_eq!(output.reserved_bytes(), survivor.len());
        assert_eq!(budget.used(), sibling.bytes() + survivor.len());
        assert_eq!(
            allocation.bytes_current,
            i64::try_from(budget.used()).unwrap() - i64::try_from(before).unwrap()
        );
        assert_eq!(*output, ScalarExpr::Column(survivor.into()));
        drop((output, sibling));
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn admitted_qualification_preserves_three_valued_child_boundaries_and_cancellation() {
        let source = ScalarExpr::Or(vec![
            ScalarExpr::Not(Box::new(ScalarExpr::And(vec![
                ScalarExpr::Column("effect".into()),
                ScalarExpr::Literal(Value::Null),
            ]))),
            ScalarExpr::Literal(Value::Bool(false)),
        ]);
        for mode in 0..3 {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let input = source.copy_for_rewrite(&control).unwrap();
            let sibling = budget.reserve(37).unwrap();
            if mode == 1 {
                original.cancel();
            }
            if mode == 2 {
                invoking.cancel();
            }
            let output = simplify_owned(input, &control, &control);
            if mode == 0 {
                let output = output.unwrap();
                let ScalarExpr::Or(items) = &source else {
                    unreachable!()
                };
                assert_eq!(*output, items[0]);
                drop(output);
            } else {
                assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(budget.used(), sibling.bytes());
            drop(sibling);
            assert_eq!(budget.used(), 0);
        }
    }

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
