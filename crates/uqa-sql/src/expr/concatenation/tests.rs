//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected output shares the invoking allowance and session presentation.

use super::*;
use crate::ast::{FunctionDispatch, FunctionVolatility};
use crate::expr::{EngineHook, IntervalStyleScope};
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalIntervalStyle, TemporalValue};

struct Session;

impl EngineHook for Session {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn temporal_interval_style(&self) -> Result<Option<TemporalIntervalStyle>> {
        Ok(Some(TemporalIntervalStyle::Iso8601))
    }
}

#[test]
fn selected_concatenation_preserves_output_style_volatility_and_null_roles() {
    let _outer = IntervalStyleScope::enter(TemporalIntervalStyle::Postgres);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut binding = FunctionBinding::dispatched(FunctionDispatch::Concat { oid: Some(2779) });
    binding.argument_types = vec!["text".into(), "interval".into()];
    let args = [
        Value::Str("x".into()),
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 1,
            micros: 7_200_000_000,
        }),
    ];
    let context = EvalContext::new(None, &[]).with_engine(&Session);
    let output = evaluate(&binding, Some(2779), &args, &control, &context).unwrap();
    // Captured PostgreSQL 18.4 runtime case under iso_8601.
    assert_eq!(*output, Value::Str("xP1DT2H".into()));
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        crate::expr::temporal_interval_style(),
        TemporalIntervalStyle::Postgres
    );
    for (dispatch, strict, volatility) in [
        (
            FunctionDispatch::Concat { oid: Some(654) },
            true,
            FunctionVolatility::Immutable,
        ),
        (
            FunctionDispatch::Concat { oid: Some(2779) },
            true,
            FunctionVolatility::Stable,
        ),
        (
            FunctionDispatch::Concat { oid: Some(2780) },
            true,
            FunctionVolatility::Stable,
        ),
        (
            FunctionDispatch::ArrayConcat,
            false,
            FunctionVolatility::Immutable,
        ),
        (
            FunctionDispatch::ArrayAppend,
            false,
            FunctionVolatility::Immutable,
        ),
        (
            FunctionDispatch::ArrayPrepend,
            false,
            FunctionVolatility::Immutable,
        ),
    ] {
        let binding = FunctionBinding::dispatched(dispatch);
        assert_eq!(
            crate::expr::bound_scalar_function_strictness(&binding.name, Some(&binding), 2),
            Some(strict)
        );
        assert_eq!(
            crate::semantics::volatility::builtin_function_volatility(
                &binding.name,
                Some(&binding),
                2
            ),
            volatility
        );
    }
}

#[test]
fn concatenation_output_checks_admission_cancellation_and_retained_identity() {
    let binding = FunctionBinding::dispatched(FunctionDispatch::Concat { oid: Some(654) });
    let values = [Value::Str("a".repeat(100)), Value::Str("b".repeat(100))];
    let context = EvalContext::new(None, &[]);
    let budget = MemoryBudget::new(8);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        evaluate(&binding, Some(654), &values, &control, &context)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            evaluate(&binding, Some(654), &values, &control, &context)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        evaluate(&binding, Some(521), &values, &control, &context)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        evaluate(&binding, Some(654), &values[..1], &control, &context)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
}
