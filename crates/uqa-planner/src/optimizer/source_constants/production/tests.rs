//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget},
    CancellationToken,
};

#[test]
fn source_value_and_label_producers_retain_exact_independent_outputs() {
    let input = ScalarExpr::Literal(Value::Str("payload".repeat(1024)));
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = BudgetedVec::new(&budget);
    let allocated = allocation_counter::measure(|| {
        observe_values(
            [&input],
            |_| Ok(control.copy_text("declared_output").unwrap()),
            None,
            &control,
            &mut |name, value| {
                output.push((name, value))?;
                Ok(())
            },
        )
        .unwrap();
    });
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].0.as_str(), "declared_output");
    let expression = output[0].1.as_ref().unwrap();
    assert!(
        matches!(&**expression, ScalarExpr::TypedLiteral { value: Value::Str(text), bound_type: Some(ColumnType::Text), .. } if text == "payload".repeat(1024).as_str())
    );
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_source_payload_production_preserves_sibling_allowance_and_both_cancellations() {
    let input = ScalarExpr::Literal(Value::Str("x".repeat(1 << 20)));
    for mode in 0..3 {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(17).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let mut error = None;
        let allocations = allocation_counter::measure(|| {
            error = Some(
                observe_values(
                    [&input],
                    |_| Ok(control.copy_text("output")?),
                    None,
                    &control,
                    &mut |_, _| panic!("failed producer publishes no output"),
                )
                .unwrap_err(),
            );
        });
        assert_eq!(
            error.unwrap().sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert!(allocations.bytes_total < 32 * 1024);
        assert_eq!(budget.used(), sibling.bytes());
    }
}
