//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn input_plan(value: Value) -> UnifiedPlan {
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0));
    let mut value = Some(value);
    plan.rewrite_scalar_expressions(&mut |node| {
        if matches!(node, ScalarExpr::Literal(Value::Int(1))) {
            *node = ScalarExpr::TypedLiteral {
                collation_oid: None,
                composite_source: None,
                value: value.take().expect("one input constant"),
                ty: "pair".into(),
                bound_type: Some(ty()),
                parameter_index: None,
            };
        }
    });
    plan
}

#[test]
fn prepared_composite_generations_share_descriptor_owners_and_release_rewritten_outputs() {
    let types = Types(RwLock::new(descriptor(2)));
    let plan = input_plan(value());
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let inputs = CompositeInputs::capture_with_control(&plan, &types, &control).unwrap();
    let captured = budget.used();
    assert!(captured > 0);
    let mut shared = None;
    let measured = allocation_counter::measure(|| {
        shared = Some(inputs.clone());
    });
    assert_eq!(measured.count_total, 0);
    drop(inputs);
    assert_eq!(budget.used(), captured);
    let inputs = shared.unwrap();
    *types.0.write().unwrap() = descriptor(3);
    let projected = inputs
        .project_logical_owned(&plan, &types, &control)
        .unwrap()
        .unwrap()
        .finish(&control)
        .unwrap();
    let mut count = 0;
    projected.visit_scalar_expressions(&mut |root| {
        root.visit(&mut |node| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Record(fields),
                composite_source,
                ..
            } = node
            {
                count += 1;
                assert_eq!(fields.values(), [Value::Int(1), Value::Null]);
                assert_eq!(composite_source.as_ref().unwrap().value, value());
            }
        });
    });
    assert_eq!(count, 1);
    assert!(budget.used() > captured);
    drop(projected);
    assert_eq!(budget.used(), captured);
    drop(inputs);
    assert_eq!(budget.used(), 0);
}

#[test]
fn composite_projection_rejects_before_copying_large_values_and_observes_both_tokens() {
    let types = Types(RwLock::new(descriptor(2)));
    let plan = input_plan(Value::Record(
        vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Str("large".repeat(256 * 1024))),
        ]
        .into(),
    ));
    let budget = MemoryBudget::new(8192);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let inputs = CompositeInputs::capture_with_control(&plan, &types, &control).unwrap();
    let captured = budget.used();
    *types.0.write().unwrap() = descriptor(3);
    let measured = allocation_counter::measure(|| {
        let error = match inputs.project_logical_owned(&plan, &types, &control) {
            Err(error) => error,
            Ok(_) => panic!("large constant must be rejected"),
        };
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 64 * 1024, "{measured:?}");
    assert_eq!(budget.used(), captured);
    for token in [&original, &invoking] {
        token.cancel();
        let error = match inputs.project_logical_owned(&plan, &types, &control) {
            Err(error) => error,
            Ok(_) => panic!("cancelled projection must fail"),
        };
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), captured);
        token.reset();
    }
    drop(inputs);
    assert_eq!(budget.used(), 0);
}
